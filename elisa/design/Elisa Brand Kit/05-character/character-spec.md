# Elisa character: Shade (P2)

A small ink-black creature with big pale eyes and tall fox ears. No mouth: eyes and ears carry every expression. Pupils sit slightly high so it reads as curious. Cyan inside the ears ties it to model activity.

## Builds

- Full (≥ 32 px): rim #3A3A46, eye highlight, paws, props.
- Mini (≤ 24 px): body #3A3A46, no rim/highlight/paws/props, vertical motion ×0.45.
- Template (menu bar 16/18/22): one-colour silhouette, eyes cut out, pupils kept. Black on light menu bar, white on dark.

## Expressions

open · wide · happy · closed · sad · glow (memory).

## States (20)

- **idle** (core, 4000 ms, loop): Waiting for input. Slow breath, a blink, one ear flick.
- **listening** (core, 2000 ms, loop): User is typing. Head tilts toward the input, small nods.
- **thinking** (core, 2400 ms, loop): Model is reasoning. Looks up, eyes narrow, three cyan beads.
- **streaming** (core, 2400 ms, loop): Writing a reply. Bobs on the beat while lines type out.
- **running** (core, 1600 ms, loop): A tool is executing. Holds a small terminal, watches progress.
- **permission** (core, 2000 ms, loop): Needs a yes or no. Sits up, paw raised, looks at you.
- **success** (core, 1600 ms, loop): Task done. Squat, hop, happy eyes, green check.
- **error** (core, 2000 ms, loop): Something failed. Ears droop, a small head shake, red badge.
- **interrupted** (core, 1400 ms, loop): User pressed stop. Startled hop back, freeze, settle.
- **memory** (core, 2400 ms, loop): A stored fact comes back. Cyan bead rises, sinks in, eyes glow.
- **sleeping** (core, 4000 ms, loop): Weights moved to disk. Curled low, eyes shut, slow z.
- **waking** (core, 2400 ms, loop): Weights back in memory. Stretches up, double blink, ears pop.
- **greeting** (action, 2000 ms, loop): Session start, waking, or a click. Hops, waves a paw, happy eyes.
- **reading** (action, 2400 ms, loop): Read tool, or summarizing. Holds a page, eyes scan line by line.
- **searching** (action, 2000 ms, loop): Grep or search. Sweeps a magnifier across, eyes follow it.
- **editing** (action, 1600 ms, loop): Edit or Write tool. Scribbles on a notepad with a pencil.
- **building** (action, 1200 ms, loop): cargo build or check. Hammers a block, sparks fly on each hit.
- **testing** (action, 2000 ms, loop): Running tests. Watches a bubbling flask, leans in.
- **celebrating** (action, 1800 ms, loop): A task finished after real work. Big jump, both paws up, confetti.
- **confused** (action, 2400 ms, loop): Unknown command or an unclear request. Head tilts side to side, question marks.

## Files

- shade/svg/elisa-shade-<state>-200.svg, -24.svg: key poses (full, mini)
- shade/svg/elisa-shade-menubar-{light,dark}-{16,18,22}.svg
- shade/png/elisa-shade-<state>-512.png, elisa-shade-states-sheet.png
- shade/motion/elisa-shade-<state>.json: keyframes
- renderer/elisa-shade.js (load elisa-character.js first): ElisaShade.svg(state, t, {size, detail:'full'|'mini'|'template', color}) returns an SVG for any t 0–1, for more frames or sprite sheets.
- alt-tanuki/: the M7 Tanuki alternative (12 core states).

## Colours

- body: #141418
- rim: #3A3A46
- miniBody: #3A3A46
- paw: #1C1C22
- earInner: #82C8DC
- eye: #EEF0E6
- pupil: #0E0E12
- effectCyan: #82C8DC
- propAmber: #E6BE50
- propCoral: #D77757
- propGreen: #78C878
- propPink: #FF8FB1
- paper: #EEF0E6

Cyan = model activity. Green, red, amber = outcomes only.
