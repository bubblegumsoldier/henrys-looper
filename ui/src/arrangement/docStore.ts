//! The one document both views edit, and its history.
//!
//! ```text
//!   Grid ── op(doc) ──► render ──► text + doc + errors ─┐
//!                                                        ├─► snapshot ─► history (undo/redo)
//!   Text ── typing ──► (debounce) parse ──► doc|errors ─┘
//! ```
//!
//! **The text is the score** - it is what is saved, loaded and compiled. The structured document is
//! its model: an edit in the grid is applied to the model and turned back into text by the backend
//! (`scoreApi.render`, canonical layout, comments kept); typing in the text view is parsed back into
//! the model (`scoreApi.parse`). Every finished step is one *snapshot* - text, model, the compiler's
//! issues - and undo/redo walk those snapshots, so ⌘Z undoes a grid edit and a burst of typing alike.
//!
//! **When the text does not parse**, the model stays at the last state that did, and `inSync` turns
//! false: the grid draws that state read-only and says so, while the text view shows the errors.
//! An edit that the compiler rejects (a `repeat:` cycle made in the grid) is different: its model
//! *is* the text's model, only invalid, so the grid stays editable and marks the faulty element.
//!
//! Nothing here knows React or Tauri; the backend is passed in, which is what lets the unit tests
//! drive the history with a fake one.

import type { CompiledScore, ModelIssue, ParseOutcome, RenderOutcome, ScoreDoc } from "../types";
import { resolveDoc } from "./resolve";

export interface Backend {
  parse: (yaml: string) => Promise<ParseOutcome>;
  render: (doc: ScoreDoc) => Promise<RenderOutcome>;
}

export interface Snapshot {
  text: string;
  /** Model of `text` when `inSync`, else the last model that parsed. Null before anything parsed. */
  doc: ScoreDoc | null;
  /** True when `doc` is the model of `text`, i.e. the grid may edit it. */
  inSync: boolean;
  /** The compiler's issues with `text`; empty means the score is valid. */
  errors: ModelIssue[];
  /** Not errors: written correctly, not supported yet (`loop: midi`, `grid`). */
  notes?: ModelIssue[];
  score: CompiledScore | null;
}

export interface DocState extends Snapshot {
  canUndo: boolean;
  canRedo: boolean;
  /** A render or parse is on the wire. */
  busy: boolean;
  /** The backend could not be reached (no app window). */
  failure: string | null;
  /** Increases on every change of `text` that did not come from the text editor itself. */
  textRev: number;
  /**
   * Which document this is: increases with every `replace` (open, new, a file changed elsewhere),
   * never with an edit or an undo. The mixer uses it to tell "this song, changed" from "another
   * song" (`mixer/scoreMix.ts`).
   */
  docId: number;
}

export interface StoreOptions {
  /** Called with every new text, from either view - the file layer persists it. */
  onText?: (text: string) => void;
  /** Delay between the last keystroke and the parse. */
  parseDelayMs?: number;
  /** Most snapshots kept. */
  limit?: number;
}

const EMPTY: Snapshot = { text: "", doc: null, inSync: false, errors: [], score: null };

export class DocStore {
  private backend: Backend;
  private opts: Required<StoreOptions>;
  private history: Snapshot[] = [EMPTY];
  private index = 0;
  private state: DocState;
  private listeners = new Set<(s: DocState) => void>();
  private parseTimer: ReturnType<typeof setTimeout> | null = null;
  /** Ticket of the newest request; an older answer must not overwrite a newer state. */
  private ticket = 0;
  /** Renders run one after the other, in the order the edits were made. */
  private chain: Promise<unknown> = Promise.resolve();
  private pending = 0;

  constructor(backend: Backend, options: StoreOptions = {}) {
    this.backend = backend;
    this.opts = { onText: () => undefined, parseDelayMs: 350, limit: 200, ...options };
    this.state = { ...EMPTY, canUndo: false, canRedo: false, busy: false, failure: null, textRev: 0, docId: 0 };
  }

  get(): DocState {
    return this.state;
  }

  subscribe(listener: (s: DocState) => void): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  private set(change: Partial<DocState>) {
    this.state = {
      ...this.state,
      ...change,
      canUndo: this.canUndo(change.text ?? this.state.text),
      canRedo: this.index < this.history.length - 1,
      busy: this.pending > 0 || this.parseTimer !== null,
    };
    for (const l of this.listeners) l(this.state);
  }

  private canUndo(text: string): boolean {
    return this.index > 0 || text !== this.history[this.index].text;
  }

  private push(snapshot: Snapshot) {
    this.history = this.history.slice(0, this.index + 1);
    this.history.push(snapshot);
    if (this.history.length > this.opts.limit) this.history.shift();
    this.index = this.history.length - 1;
  }

  private cancelParse() {
    if (this.parseTimer !== null) {
      clearTimeout(this.parseTimer);
      this.parseTimer = null;
    }
  }

  /** A whole new text (open, new, a file changed elsewhere): history starts over. */
  async replace(text: string): Promise<void> {
    this.cancelParse();
    const ticket = ++this.ticket;
    this.history = [{ ...EMPTY, text }];
    this.index = 0;
    this.set({ text, textRev: this.state.textRev + 1, docId: this.state.docId + 1 });
    const snap = await this.parsed(text, null);
    if (ticket !== this.ticket || snap === null) return;
    this.history = [snap];
    this.index = 0;
    this.set({ ...snap, failure: null });
  }

  /** Parse `text` into a snapshot, keeping `fallback` as the model when it does not parse. */
  private async parsed(text: string, fallback: ScoreDoc | null): Promise<Snapshot | null> {
    if (text.trim() === "") return { text, doc: fallback, inSync: false, errors: [], score: null };
    this.pending++;
    try {
      const out = await this.backend.parse(text);
      if (out.ok && out.doc) return { text, doc: out.doc, inSync: true, errors: [], notes: out.notes ?? [], score: out.score };
      return { text, doc: fallback, inSync: false, errors: out.errors, notes: out.notes ?? [], score: null };
    } catch (e) {
      this.set({ failure: (e as Error).message });
      return null;
    } finally {
      this.pending--;
    }
  }

  /**
   * The text editor changed the text. The text is taken at once (it is the file); the model
   * follows after a pause in typing, and that pause is also one step in the history.
   */
  setText(text: string): void {
    if (text === this.state.text) return;
    this.cancelParse();
    const ticket = ++this.ticket;
    this.opts.onText(text);
    this.parseTimer = setTimeout(() => {
      this.parseTimer = null;
      void this.commitText(text, ticket);
    }, this.opts.parseDelayMs);
    this.set({ text });
  }

  private async commitText(text: string, ticket: number) {
    const fallback = this.lastGoodDoc();
    const snap = await this.parsed(text, fallback);
    if (snap === null) return;
    if (ticket !== this.ticket) {
      this.set({});
      return;
    }
    this.push(snap);
    this.set({ ...snap });
  }

  private lastGoodDoc(): ScoreDoc | null {
    return this.state.doc;
  }

  /** Parse what was typed now rather than after the pause (before a save or a load). */
  async flush(): Promise<void> {
    if (this.parseTimer === null) return;
    this.cancelParse();
    await this.commitText(this.state.text, this.ticket);
  }

  /**
   * Apply a grid edit. Refused (false) while the model is not the text's - the grid is read-only
   * then. The edited model is shown at once; text and issues follow from the render.
   */
  edit(op: (doc: ScoreDoc) => ScoreDoc): boolean {
    const doc = this.state.doc;
    if (!doc || !this.state.inSync || this.parseTimer !== null) return false;
    const next = op(doc);
    if (next === doc) return true;
    const ticket = ++this.ticket;
    this.pending++;
    this.set({ doc: resolveDoc(next) });
    this.chain = this.chain.then(async () => {
      try {
        const out = await this.backend.render(next);
        const snap: Snapshot = {
          text: out.yaml,
          doc: out.doc,
          inSync: true,
          errors: out.ok ? [] : out.errors,
          notes: out.notes ?? [],
          score: out.score,
        };
        this.push(snap);
        this.opts.onText(out.yaml);
        this.pending--;
        if (ticket === this.ticket) this.set({ ...snap, textRev: this.state.textRev + 1, failure: null });
        else this.set({});
      } catch (e) {
        this.pending--;
        // Back to what the text says; the edit did not happen.
        const current = this.history[this.index];
        if (ticket === this.ticket) this.set({ ...current, failure: (e as Error).message });
      }
    });
    return true;
  }

  /** Resolves once every edit made so far has been rendered. */
  settled(): Promise<void> {
    return this.chain.then(() => undefined);
  }

  undo(): void {
    if (this.pending > 0) return;
    // Typing that has not been parsed yet is the newest step: undo drops it first.
    if (this.state.text !== this.history[this.index].text) {
      this.cancelParse();
      ++this.ticket;
      this.restore(this.history[this.index]);
      return;
    }
    if (this.index === 0) return;
    ++this.ticket;
    this.index--;
    this.restore(this.history[this.index]);
  }

  redo(): void {
    if (this.pending > 0 || this.parseTimer !== null) return;
    if (this.index >= this.history.length - 1) return;
    ++this.ticket;
    this.index++;
    this.restore(this.history[this.index]);
  }

  private restore(snap: Snapshot) {
    this.opts.onText(snap.text);
    this.set({ ...snap, textRev: this.state.textRev + 1 });
  }
}
