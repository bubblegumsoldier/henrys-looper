# Altformat (Python-Prototyp mit Ableton)

Diese Partituren gehoeren zum alten Python-Prototyp (`looper/`, `backend/`), der Ableton Live
fernsteuerte. Sie benutzen Felder wie `ableton_track`, `group`, `layers` und `reserve`, die der
Rust-Compiler der Desktop-App nicht mehr kennt - er lehnt sie mit einer Meldung ab.

Fuer die App: `examples/henry-3-4.rust.yaml` (Henrys Song), `examples/solo-loop-4-4.yaml`,
`examples/waltz-three-tracks-3-4.yaml`.
