(function () {
  const STATE_SCENES = { idle: 'Idle', listening: 'Listening', thinking: 'Thinking', streaming: 'Streaming', running: 'Running', permission: 'Permission', success: 'Success', error: 'Error', interrupted: 'Interrupted', memory: 'Memory', sleeping: 'Sleeping', waking: 'Waking' };
  const KEY_T = { idle: .5, listening: .5, thinking: .6, streaming: .62, running: .6, permission: .5, success: .45, error: .5, interrupted: .3, memory: .4, sleeping: .35, waking: .85 };
  const MOTION = {
    enter: (T, start, d) => Easing.easeOutCubic(clamp((T - start) / (d || .5), 0, 1)),
    pop: (T, start, d) => Easing.easeOutBack(clamp((T - start) / (d || .5), 0, 1)),
    fade: (T, start, d) => Easing.easeInOutSine(clamp((T - start) / (d || .3), 0, 1))
  };
  const MONO = "'Chivo Mono', monospace", SANS = "'Chivo Mono', ui-monospace, monospace";

  function Shade({ state, t, size, opacity }) {
    const html = window.ElisaShade.svg(state, t, { size });
    return <div style={{ position: 'absolute', inset: 0, opacity, display: 'flex', alignItems: 'center', justifyContent: 'center' }} dangerouslySetInnerHTML={{ __html: html }} />;
  }

  function Piece({ theme }) {
    const { T, CUES, authoredTotal } = useComposition();
    const ET = window.ElisaShade, order = ET.ORDER;
    const light = theme === 'light';
    const ink = light ? '#16131C' : '#ECE8F2', sec = light ? '#5E566C' : '#A9A2B6', line = light ? '#DAD3E4' : '#3F3850';
    const starts = order.map(k => CUES[STATE_SCENES[k]]);
    const endStates = CUES.Close;
    let i = 0; for (let k = 0; k < order.length; k++) if (T >= starts[k]) i = k;
    const inStates = T >= starts[0] && T < endStates;
    const local = st => (T - CUES[STATE_SCENES[st]]) * 1000 / ET.STATES[st].dur;

    // Character layers: opening idle, 12 states, close success→idle; 0.3 s crossfades at each cue.
    const layers = [];
    if (T < starts[0] + .15) layers.push({ key: 'open', st: 'idle', t: T * 1000 / 4000, o: 1 - MOTION.fade(T, starts[0] - .15, .3) });
    order.forEach((st, k) => {
      const a = starts[k], b = k < order.length - 1 ? starts[k + 1] : endStates;
      if (T < a - .15 || T > b + .15) return;
      layers.push({ key: st, st, t: local(st), o: MOTION.fade(T, a - .15, .3) * (1 - MOTION.fade(T, b - .15, .3)) });
    });
    if (T > endStates - .15) layers.push({ key: 'close', st: 'idle', t: (T - endStates) * 1000 / 4000, o: MOTION.fade(T, endStates - .15, .3) });

    const intro = MOTION.pop(T, .2, .7), outro = 1 - MOTION.enter(T, authoredTotal - .6, .6);
    const charScale = intro * outro * (1.0 + .02 * Math.sin(2 * Math.PI * T / authoredTotal));

    const titleIn = MOTION.enter(T, .5, .6) * (1 - MOTION.fade(T, starts[0] - .4, .4));
    const closeIn = MOTION.enter(T, endStates + .2, .6) * outro;
    const S = ET.STATES[order[i]];
    const a = starts[i], b = i < order.length - 1 ? starts[i + 1] : endStates;
    const txt = inStates ? MOTION.enter(T, a, .45) * (1 - MOTION.fade(T, b - .3, .3)) : 0;
    const railIn = MOTION.enter(T, starts[0] - .4, .5) * (1 - MOTION.fade(T, endStates + 1.2, .5));

    const rail = React.useMemo(() => order.map(st => ({ st, html: ET.svg(st, KEY_T[st], { size: 76, noProps: true }) })), []);

    return (
      <div data-screen-label={`t=${Math.floor(T)}s`} style={{ position: 'absolute', inset: 0, background: light ? '#F8F6FB' : 'radial-gradient(90% 80% at 32% 70%, #2A2536 0%, #16131C 65%)', color: ink, fontFamily: SANS, overflow: 'hidden' }}>
        <div style={{ position: 'absolute', left: 96, top: 80, fontFamily: "'Silkscreen', monospace", fontSize: 26, letterSpacing: '.06em', color: sec }}>ELISA / CHARACTER / P2 SHADE</div>

        <div style={{ position: 'absolute', left: 120, top: 150, width: 760, height: 760, transform: `scale(${charScale})`, transformOrigin: '50% 86%' }}>
          {layers.map(l => <Shade key={l.key} state={l.st} t={l.t} size={760} opacity={l.o} />)}
        </div>

        <div style={{ position: 'absolute', left: 1000, top: 360, width: 820, opacity: titleIn, transform: `translateY(${(1 - titleIn) * 24}px)` }}>
          <div style={{ fontSize: 112, fontWeight: 500, letterSpacing: '-.02em', lineHeight: 1.02 }}>Meet Shade</div>
          <div style={{ marginTop: 28, fontSize: 38, lineHeight: 1.4, color: sec }}>Elisa’s companion on your Mac. Twelve states, one renderer.</div>
        </div>

        <div style={{ position: 'absolute', left: 1000, top: 300, width: 820, opacity: txt, transform: `translateY(${(1 - txt) * 20}px)` }}>
          <div style={{ display: 'flex', gap: 24, fontFamily: MONO, fontSize: 28 }}>
            <span style={{ color: light ? '#C2386A' : '#FF8FB1', fontFamily: "'Silkscreen', monospace" }}>{String(i + 1).padStart(2, '0')} / 12</span>
            <span style={{ color: light ? '#1D6E84' : '#9FE0F0' }}>{S.dur} ms · loop</span>
          </div>
          <div style={{ marginTop: 24, fontSize: 104, fontWeight: 500, letterSpacing: '-.02em', lineHeight: 1.04, textWrap: 'balance' }}>{S.label}</div>
          <div style={{ marginTop: 28, fontSize: 40, lineHeight: 1.4, color: sec, textWrap: 'pretty' }}>{S.use}</div>
        </div>

        <div style={{ position: 'absolute', left: 1000, top: 380, width: 820, opacity: closeIn, transform: `translateY(${(1 - closeIn) * 24}px)` }}>
          <div style={{ fontSize: 96, fontWeight: 500, letterSpacing: '-.02em', lineHeight: 1.05 }}>Ships as SVG and JSON</div>
          <div style={{ marginTop: 28, fontFamily: MONO, fontSize: 30, lineHeight: 1.6, color: sec }}>elisa-shade.js<br />motion/shade/*.json<br />svg/shade/*.svg</div>
        </div>

        <div style={{ position: 'absolute', left: 96, right: 96, bottom: 56, display: 'grid', gridTemplateColumns: 'repeat(12, 1fr)', gap: 8, opacity: railIn }}>
          {rail.map((r, k) => {
            const on = inStates && k === i;
            return (
              <div key={r.st} style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 6, paddingBottom: 10, borderBottom: `4px solid ${on ? (light ? '#C2386A' : '#FF8FB1') : line}`, opacity: on ? 1 : .55 }}>
                <div dangerouslySetInnerHTML={{ __html: r.html }} />
                <span style={{ fontFamily: MONO, fontSize: 24, color: on ? ink : sec }}>{String(k + 1).padStart(2, '0')}</span>
              </div>
            );
          })}
        </div>
      </div>
    );
  }

  function ShadeReel() {
    const [t, setTweak] = useTweaks(window.TWEAK_DEFAULTS);
    return (
      <React.Fragment>
        <CompositionStage width={1920} height={1080} scenes={window.OM_SCENES} playback={window.OM_PLAYBACK} bg={t.theme === 'light' ? '#F8F6FB' : '#16131C'}>
          <Piece theme={t.theme} />
        </CompositionStage>
        <TweaksPanel>
          <TweakSection label="Reel" />
          <TweakToggle label="Motion editor" value={t.motionEditor} onChange={v => setTweak('motionEditor', v)} />
          <TweakRadio label="Background" value={t.theme} options={['dark', 'light']} onChange={v => setTweak('theme', v)} />
        </TweaksPanel>
      </React.Fragment>
    );
  }
  window.ShadeReel = ShadeReel;
})();
