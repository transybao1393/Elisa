# Layout and alignment

```
┌──────────────────────────────────────────────────────────────┐ radius 12
│ ● ● ●  ELISA  path · model                        [button]  │ title bar 38, pad-x 14
├────────────────────────────────────────────────┬─────────────┤
│ pad 20 / 28 / 6, gap 16 between blocks         │ rail 180    │
│                                   ┌─────────┐  │ pad 18/16   │
│                                   │VERB file│  │ gap 18      │
│                                   └───▽─────┘  │             │
│                                    (Shade)     │             │
│                            right 16, bottom 0  │             │
├────────────────────────────────────────────────┴─────────────┤
│ pad 0 / 24 / 12                                              │
│ [16px Shade] status · status · status           height 22    │
│ ┌ input: 1px accent, radius 8, pad 11/14 ──────────────────┐ │
│ │ ›  placeholder                                           │ │
│ └──────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────┘
```

- Transcript is bottom-anchored; content grows upward from the input.
- Text never sits under the docked companion: transcript padding-right = size + 28.
- The "more below" pill sits left of the companion.
- Below 1100 px window width the companion caps at 96 px.

## Character grid

- Artboard 200 × 200, ground line y 172, body centred on x 100, body 104 wide (48–152), y 66–172.
- Eyes centred (75, 112) and (125, 112), 42 × 48.
- Pivots: body (100, 172) for squash/stretch/rotation; ears (74, 80) and (126, 80); right paw (118, 170).
- Props may extend past the artboard. Keep 24 px clear space around the character in UI.
- Mini build (≤ 24 px): viewBox 27 32 146 146, vertical motion ×0.45.
