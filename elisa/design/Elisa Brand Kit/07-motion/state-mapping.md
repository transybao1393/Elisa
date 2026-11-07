# When each state plays

| App event | State | Attention pop* |
|---|---|---|
| Launch, wake, click on Elisa | greeting | yes |
| User typing | listening | |
| Model reasoning | thinking | |
| Tokens streaming | streaming | |
| Read tool, summarizing | reading | |
| Grep / search / glob | searching | |
| Edit / Write | editing | |
| build / check / clippy | building | |
| Test command | testing | |
| Other tool | running | |
| Waiting for approval | permission | yes |
| Reply finished, no tools | success | |
| Reply finished after tools | celebrating | yes |
| Unknown command | confused | yes |
| Tool failed | error | yes |
| Esc | interrupted | |
| Memory recalled | memory | |
| Idle timeout / model unloaded | sleeping | |
| Model reloading | waking | yes |

*Docked companion scales 1.22× (spring cubic-bezier(.34,1.7,.64,1), 350 ms), bubble lifts 6 px with accent border, radial glow behind (accent; warn for permission; error for error).

## Bubble copy

VERB in Silkscreen + target in Chivo Mono: READING split.rs · EDITING split.rs · TESTING cargo test · NEEDS YOU cargo test · ALL DONE! tests green · HUH? try /help · HI! ready when you are.

## Motion JSON format

`tracks` keyed by channel (x, y, sx, sy, rot, earL, earR, eyeOpen, lookX, lookY, eye, pawR, glow, dim, p1–p3). Keyframes are {t, ms, value, ease}; t is 0–1 of durationMs; ease names in easing.json. Missing channels use the default pose.
