//! The mixer of the Arrangement mode (design/arrangement-mixer.html): one strip per track, a gap,
//! the two bus strips. Docked under the arrangement with a draggable top edge, or filling the whole
//! editor area; "Buses only" hides the track strips. Selecting a strip selects the channel the
//! detail panel on the right shows.

import { useCallback, useMemo, useRef, type PointerEvent as ReactPointerEvent } from "react";
import { currentStatus, structureSignature, useStatusSlice } from "../status";
import { Icon } from "../shell/Icon";
import { T } from "../shell/terms";
import { useEngine } from "../shell/engine";
import { channelActions } from "./actions";
import { BusStrip, busSignature, type BusGlobals, type BusView } from "./BusStrip";
import { Strip } from "./Strip";
import { DEFAULT_HEIGHT, MIN_HEIGHT, selectChannel, setMixer, useMixer } from "./store";

export function Mixer() {
  const m = useMixer();
  const running = useStatusSlice((s) => s.ready);
  const signature = useStatusSlice(structureSignature);
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const tracks = useMemo(() => currentStatus().tracks, [signature]);
  const busSig = useStatusSlice(busSignature);
  const { buses, globals } = useMemo(() => {
    const s = currentStatus();
    const buses: BusView[] = s.buses.map((b) => ({
      id: b.id,
      channel: b.channel,
      width: b.width,
      channelLabel: b.channel_label,
      gain: b.gain,
      hasClick: b.has_click,
    }));
    const globals: BusGlobals = {
      click: s.click,
      clickGain: s.click_gain,
      outputChannels: s.output_channels,
      collapsed: s.buses_collapsed,
    };
    return { buses, globals, note: s.bus_note };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [busSig]);
  const busNote = useStatusSlice((s) => s.bus_note);
  const engine = useEngine();
  const anySolo = tracks.some((t) => t.soloed);
  const resizing = useRef<{ y: number; h: number } | null>(null);
  const section = useRef<HTMLElement | null>(null);

  const onSelect = useCallback((t: number) => selectChannel(t, true), []);

  const startResize = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (m.full) return;
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    resizing.current = { y: e.clientY, h: section.current?.getBoundingClientRect().height ?? m.height };
  };
  const moveResize = (e: ReactPointerEvent<HTMLDivElement>) => {
    const r = resizing.current;
    if (!r) return;
    const parent = section.current?.parentElement?.getBoundingClientRect().height ?? 900;
    const h = Math.round(Math.min(parent - 80, Math.max(MIN_HEIGHT, r.h + (r.y - e.clientY))));
    if (section.current) section.current.style.height = `${h}px`;
  };
  const endResize = (e: ReactPointerEvent<HTMLDivElement>) => {
    const r = resizing.current;
    if (!r) return;
    resizing.current = null;
    const h = section.current?.getBoundingClientRect().height ?? m.height;
    setMixer({ height: Math.round(h) });
    e.currentTarget.releasePointerCapture(e.pointerId);
  };

  const showTracks = m.show === "all";

  return (
    <section
      className={`mixer${m.full ? " full" : ""}`}
      ref={section}
      style={m.full ? undefined : { height: m.height }}
      aria-label={T.mixer}
    >
      {!m.full && (
        <div
          className="mixer-grip"
          onPointerDown={startResize}
          onPointerMove={moveResize}
          onPointerUp={endResize}
          onDoubleClick={() => setMixer({ height: DEFAULT_HEIGHT })}
          title="Drag to resize the mixer · double-click for the default height"
          role="separator"
          aria-orientation="horizontal"
        />
      )}
      <div className="mixer-h">
        <span className="caps ttl">
          <Icon name="sliders" size={12} /> {T.mixer}
        </span>
        <span className="dim cnt">
          {tracks.length} {tracks.length === 1 ? "track" : "tracks"} · {buses.length} buses
        </span>
        {anySolo && (
          <button className="solo-clear" onClick={() => tracks.forEach((t) => t.soloed && channelActions.solo(t.index, false))} title="Release every solo">
            Solo active · clear
          </button>
        )}
        {busNote && (
          <span className="busnote" title={busNote}>
            <Icon name="warn" size={11} /> Main and Monitor share outputs
          </span>
        )}
        <div className="grow" />
        <div className="seg" role="radiogroup" aria-label="Strips">
          <button className={showTracks ? "on" : ""} onClick={() => setMixer({ show: "all" })} aria-checked={showTracks} role="radio">
            Tracks + buses
          </button>
          <button className={!showTracks ? "on" : ""} onClick={() => setMixer({ show: "buses" })} aria-checked={!showTracks} role="radio">
            Buses only
          </button>
        </div>
        <button
          className={`btn s sq ghost${m.full ? " on" : ""}`}
          onClick={() => setMixer({ full: !m.full })}
          title={m.full ? "Dock the mixer under the arrangement" : "Mixer as full view"}
          aria-pressed={m.full}
        >
          <Icon name={m.full ? "split" : "grid"} />
        </button>
        <button className="btn s sq ghost" onClick={() => setMixer({ open: false, full: false })} title="Close the mixer">
          <Icon name="x" />
        </button>
      </div>
      {!running ? (
        <div className="mixer-off">
          <Icon name="power" size={18} />
          <span>{engine.busy ? "The engine is starting…" : "No setup yet - add tracks in Setup."}</span>
        </div>
      ) : (
        <div className="strips">
          {showTracks &&
            tracks.map((t) => (
              <Strip
                key={t.index}
                track={t}
                selected={t.index === m.selected && m.bus === null}
                implicitMute={anySolo && !t.soloed}
                onSelect={onSelect}
                actions={channelActions}
              />
            ))}
          {showTracks && <div className="strip-gap" />}
          {buses.map((b) => (
            <BusStrip key={b.id} bus={b} g={globals} selected={m.bus === b.id} />
          ))}
          {showTracks && tracks.length < 8 && (
            <div className="strip ghoststrip" title="Tracks are set up in Setup → Tracks; a new track restarts the engine.">
              <span>
                <Icon name="plus" size={16} />
              </span>
              <b>Track</b>
              <span className="dim2">{8 - tracks.length} free</span>
            </div>
          )}
        </div>
      )}
    </section>
  );
}
