(function () {
  const EC = window.ElisaChar;
  const { ORDER, EASE, EASE_INFO, pose, rr, mix } = EC;
  const ACT = {
    greeting: { label: 'Greeting', dur: 2000, loop: true, use: 'Session start, waking, or a click. Hops, waves a paw, happy eyes.', tracks: {
      y: [[0, 0], [.1, -10, 'out'], [.25, 0, 'in'], [1, 0]], pawR: [[0, 0, 'out'], [.15, 1], [.85, 1, 'in'], [1, 0]],
      p1: [[0, 0], [.2, 0], [.3, 1], [.4, -1], [.5, 1], [.6, -1], [.7, 1], [.8, 0], [1, 0]], eye: [[0, 'block', 'step'], [.2, 'happy'], [.85, 'block']],
      earL: [[0, 0], [.15, -12, 'back'], [.85, -12], [1, 0]], earR: [[0, 0], [.15, -12, 'back'], [.85, -12], [1, 0]], rot: [[0, 0], [.3, -4], [.5, 4], [.7, -4], [.85, 0], [1, 0]] } },
    reading: { label: 'Reading', dur: 2400, loop: true, use: 'Read tool, or summarizing. Holds a page, eyes scan line by line.', tracks: {
      lookY: [[0, 6]], lookX: [[0, -6, 'lin'], [.23, 6], [.25, -6, 'lin'], [.48, 6], [.5, -6, 'lin'], [.73, 6], [.75, -6, 'lin'], [.98, 6], [1, -6]], eyeOpen: [[0, .85]], rot: [[0, 2]], p1: [[0, 0, 'lin'], [1, 1]] } },
    searching: { label: 'Searching', dur: 2000, loop: true, use: 'Grep or search. Sweeps a magnifier across, eyes follow it.', tracks: {
      p1: [[0, 0], [.5, 1], [1, 0]], lookX: [[0, -7], [.5, 7], [1, -7]], lookY: [[0, 1]], rot: [[0, -3], [.5, 3], [1, -3]], earL: [[0, -6]], earR: [[0, -6]] } },
    editing: { label: 'Editing', dur: 1600, loop: true, use: 'Edit or Write tool. Scribbles on a notepad with a pencil.', tracks: {
      lookY: [[0, 6]], lookX: [[0, 5]], eyeOpen: [[0, .8]], p1: [[0, 0, 'lin'], [1, 1]], y: [[0, 0], [.25, 1.5], [.5, 0], [.75, 1.5], [1, 0]], rot: [[0, 4]], pawR: [[0, .55]] } },
    building: { label: 'Building', dur: 1200, loop: true, use: 'cargo build or check. Hammers a block, sparks fly on each hit.', tracks: {
      p1: [[0, 0, 'in'], [.36, 1], [.5, 1], [1, 0]], p2: [[0, 1], [.36, 0, 'out'], [.8, 1], [1, 1]],
      sy: [[0, 1], [.36, 1, 'out'], [.42, .94, 'out'], [.6, 1], [1, 1]], sx: [[0, 1], [.36, 1, 'out'], [.42, 1.04], [.6, 1], [1, 1]],
      lookY: [[0, 6]], lookX: [[0, 6]], eye: [[0, 'block', 'step'], [.36, 'closed'], [.46, 'block']], pawR: [[0, .7]] } },
    testing: { label: 'Testing', dur: 2000, loop: true, use: 'Running tests. Watches a bubbling flask, leans in.', tracks: {
      lookX: [[0, 7]], lookY: [[0, 2]], p1: [[0, 0, 'lin'], [1, 1]], rot: [[0, 3], [.5, 6], [1, 3]], earR: [[0, 0], [.5, -8], [1, 0]], earL: [[0, -4]] } },
    celebrating: { label: 'Celebrating', dur: 1800, loop: true, use: 'A task finished after real work. Big jump, both paws up, confetti.', tracks: {
      y: [[0, 0], [.1, 4, 'out'], [.32, -34, 'in'], [.55, 0, 'out'], [.64, 3], [.74, 0], [1, 0]],
      sy: [[0, 1], [.1, .86, 'out'], [.3, 1.12], [.55, .9, 'out'], [.66, 1.03], [.76, 1], [1, 1]], sx: [[0, 1], [.1, 1.1, 'out'], [.3, .94], [.55, 1.08, 'out'], [.66, .99], [.76, 1], [1, 1]],
      eye: [[0, 'happy']], earL: [[0, 0], [.3, -16], [.6, 0, 'back'], [1, 0]], earR: [[0, 0], [.3, -16], [.6, 0, 'back'], [1, 0]], pawR: [[0, 0], [.2, 1], [.7, 1], [.9, 0], [1, 0]], p1: [[0, 0], [.22, 0, 'out'], [1, 1]] } },
    confused: { label: 'Confused', dur: 2400, loop: true, use: 'Unknown command or an unclear request. Head tilts side to side, question marks.', tracks: {
      rot: [[0, 0], [.15, -10, 'back'], [.45, -10], [.6, 10, 'back'], [.9, 10], [1, 0]], lookX: [[0, 0], [.15, -6], [.45, -6], [.6, 6], [.9, 6], [1, 0]], lookY: [[0, -4]],
      earL: [[0, 0], [.15, 10], [.45, 10], [.6, -8], [.9, -8], [1, 0]], earR: [[0, 0], [.15, -8], [.45, -8], [.6, 10], [.9, 10], [1, 0]], eye: [[0, 'wide']], p1: [[0, 0, 'out'], [.18, 1], [.85, 1, 'in'], [1, 0]] } }
,
    poked: { label: 'Poked', dur: 1400, loop: true, use: 'User clicks the character. Squashes, ears flatten, wide eyes, then a happy squint.', tracks: {
      x: [[0, 0], [.06, 6, 'out'], [.2, -3], [.32, 1.5], [.45, 0], [1, 0]], sx: [[0, 1], [.06, 1.14, 'out'], [.2, .94], [.34, 1.03], [.48, 1], [1, 1]], sy: [[0, 1], [.06, .86, 'out'], [.2, 1.06], [.34, .98], [.48, 1], [1, 1]],
      rot: [[0, 0], [.06, 8, 'out'], [.2, -5], [.34, 2], [.48, 0], [1, 0]], eye: [[0, 'wide', 'step'], [.5, 'happy'], [.94, 'block']], lookX: [[0, -6], [.5, 0]],
      earL: [[0, 0], [.06, 16, 'out'], [.45, 0, 'back'], [1, 0]], earR: [[0, 0], [.06, 16, 'out'], [.45, 0, 'back'], [1, 0]], p1: [[0, 0, 'out'], [.35, 1], [1, 1]] } },
    drinking: { label: 'Drinking', dur: 2400, loop: true, use: 'User gives water. Holds the bottle, tips it up, eyes close while it gulps.', tracks: {
      p1: [[0, 0, 'lin'], [1, 1]], p2: [[0, 0], [.2, 1], [.72, 1], [.88, 0], [1, 0]], rot: [[0, -2], [.2, -7], [.72, -7], [.88, -2], [1, -2]], lookY: [[0, -6]], lookX: [[0, 4]],
      eye: [[0, 'block', 'step'], [.22, 'closed'], [.74, 'block']], y: [[0, 0], [.3, -1.5], [.38, 0], [.46, -1.5], [.54, 0], [.62, -1.5], [.7, 0], [1, 0]], earL: [[0, -4]], earR: [[0, -4]], pawR: [[0, .6]] } },
    eating: { label: 'Eating', dur: 1600, loop: true, use: 'User gives food. Leans into the box and nibbles, crumbs pop out.', tracks: {
      p1: [[0, 0, 'lin'], [1, 1]], y: [[0, 0], [.25, 4], [.5, 0], [.75, 4], [1, 0]], sy: [[0, 1], [.25, .96], [.5, 1], [.75, .96], [1, 1]], rot: [[0, 4]], lookY: [[0, 8]], lookX: [[0, 4]],
      eye: [[0, 'block', 'step'], [.2, 'happy'], [.48, 'block'], [.7, 'happy'], [.96, 'block']], earL: [[0, 0], [.25, 6], [.5, 0], [.75, 6], [1, 0]], earR: [[0, 0], [.25, 6], [.5, 0], [.75, 6], [1, 0]] } },
    playing: { label: 'Playing', dur: 2000, loop: true, use: 'User gives a toy. Bats a yarn ball, eyes follow it bouncing.', tracks: {
      p1: [[0, 0, 'lin'], [1, 1]], pawR: [[0, 0], [.12, 1, 'out'], [.28, 0], [.62, 1, 'out'], [.78, 0], [1, 0]], x: [[0, 0], [.12, 4], [.3, 0], [.62, 4], [.8, 0], [1, 0]],
      lookX: [[0, 4], [.25, 8], [.4, 5], [.5, 4], [.75, 8], [.9, 5], [1, 4]], lookY: [[0, 6], [.28, -2], [.45, 6], [.78, -2], [.95, 6], [1, 6]], earL: [[0, -10]], earR: [[0, -10]],
      eye: [[0, 'wide', 'step'], [.4, 'happy'], [.52, 'wide'], [.9, 'happy'], [1, 'wide']], rot: [[0, 2], [.12, 5], [.3, 1], [.62, 5], [.8, 1], [1, 2]] } }
  };
  const ACTIONS = Object.keys(ACT);
  const STATES = Object.assign({}, EC.STATES, { thinking: Object.assign({}, EC.STATES.thinking, { use: 'Model is reasoning. Looks up, eyes narrow, three cyan beads.' }) }, ACT);
  const ALL = ORDER.concat(ACTIONS);
  const DEF = { x: 0, y: 0, sx: 1, sy: 1, rot: 0, earL: 0, earR: 0, eyeOpen: 1, lookX: 0, lookY: 0, eye: 'block', tail: 0, pawR: 0, glow: 0, dim: 0, p1: 0, p2: 0, p3: 0 };
  const val = (k, t) => { if (k.length === 1 || t <= k[0][0]) return k[0][1]; for (let i = 0; i < k.length - 1; i++) { const a = k[i], b = k[i + 1]; if (t <= b[0]) { if (typeof a[1] !== 'number') return a[1]; const u = (t - a[0]) / ((b[0] - a[0]) || 1); return a[1] + (b[1] - a[1]) * EASE[a[2] || 'io'](u); } } return k[k.length - 1][1]; };
  const poseX = (state, u) => { if (!ACT[state]) return pose(state, u); const p = Object.assign({}, DEF); for (const k in ACT[state].tracks) p[k] = val(ACT[state].tracks[k], u); return p; };
  const n = v => Math.round(v * 100) / 100;
  const COL = Object.assign({}, EC.COL, { body: '#141418', rim: '#3A3A46', mini: '#3A3A46', paw: '#1C1C22', earIn: '#82C8DC', white: '#EEF0E6', pupil: '#0E0E12' });
  const BODY = 'M50 132C46 92 66 66 100 66C134 66 154 92 150 132C154 154 142 172 120 172H80C58 172 46 154 50 132Z';
  const EAR = 'M-15 10C-17 -20 -8 -44 -2 -46C4 -46 10 -20 15 4Z';
  const EAR_IN = 'M-8 4C-9 -16 -5 -34 -2 -36C1 -36 5 -16 8 2Z';
  const EY = 112, EX = [75, 125], RX = 21, BASE_LOOK_Y = -3;
  const cl = (v, a) => Math.max(-a, Math.min(a, v));

  function eye(x, i, p, mini, body) {
    const side = i ? 1 : -1, white = mix(COL.white, COL.cyan, p.glow * .35), ink = COL.ink;
    const sw = mini ? 6 : 4.5;
    if (p.eye === 'happy') return `<path d="M${x - 14} ${EY + 5}Q${x} ${EY - 13} ${x + 14} ${EY + 5}" fill="none" stroke="${white}" stroke-width="${sw}" stroke-linecap="round"/>`;
    if (p.eye === 'closed') return `<path d="M${x - 14} ${EY}Q${x} ${EY + 8} ${x + 14} ${EY}" fill="none" stroke="${white}" stroke-width="${sw * .85}" stroke-linecap="round"/>`;
    const wide = p.eye === 'wide', sad = p.eye === 'sad';
    const rx = RX * (wide ? 1.08 : 1), bl = Math.max(.05, Math.min(1.15, p.eyeOpen));
    if (bl < .2) return `<path d="M${x - 15} ${EY}Q${x} ${EY + 4} ${x + 15} ${EY}" fill="none" stroke="${white}" stroke-width="${sw * .8}" stroke-linecap="round"/>`;
    const ry = rx * 1.15 * bl;
    const lx = cl(p.lookX * rx / 10, rx * .42), ly = cl((p.lookY + BASE_LOOK_Y) * .9 + 2, ry * .4);
    const g = [`<ellipse cx="${x}" cy="${EY}" rx="${n(rx)}" ry="${n(ry)}" fill="${white}"${mini ? '' : ` stroke="${ink}" stroke-width="2"`}/>`];
    const prx = rx * (wide ? .42 : .55), pry = Math.min(ry, rx * 1.15) * (wide ? .46 : .58);
    const pc = mix(COL.pupil, '#1E5A6A', p.glow);
    g.push(`<ellipse cx="${n(x + lx)}" cy="${n(EY + ly)}" rx="${n(prx)}" ry="${n(pry)}" fill="${pc}"/>`);
    if (!mini) {
      g.push(`<circle cx="${n(x + lx - rx * .22)}" cy="${n(EY + ly - rx * .25)}" r="${n(rx * (wide ? .16 : .13))}" fill="#fff" opacity=".85"/>`);
      if (p.glow > .01) g.push(`<circle cx="${n(x + lx)}" cy="${n(EY + ly)}" r="${n(prx * .5)}" fill="${COL.cyan}" opacity="${n(.8 * p.glow)}"/>`);
    }
    if (sad) {
      const inner = x - side * (rx + 3), outer = x + side * (rx + 3);
      g.push(`<path d="M${inner} ${EY - ry * .55}L${outer} ${EY - ry * .05}L${outer} ${EY - ry - 6}L${inner} ${EY - ry - 6}Z" fill="${body}"/>`);
      if (!mini) g.push(`<path d="M${inner} ${n(EY - ry * .55)}L${outer} ${n(EY - ry * .05)}" stroke="${COL.rim}" stroke-width="2.5" stroke-linecap="round"/>`);
    }
    return g.join('');
  }

  function figure(state, p, mini) {
    const g = [], body = mini ? COL.mini : COL.body, st = mini ? '' : `stroke="${COL.rim}" stroke-width="2.5" stroke-linejoin="round"`;
    [[74, -p.earL, 1], [126, p.earR, -1]].forEach(([x, a, m]) => g.push(`<g transform="translate(${x} 80) rotate(${n(a)}) scale(${m} 1)"><path d="${EAR}" fill="${body}" ${st}/><path d="${EAR_IN}" fill="${COL.earIn}" opacity="${mini ? 1 : .9}"/></g>`));
    g.push(`<path d="${BODY}" fill="${body}" ${st}/>`);
    if (!mini) g.push(`<path d="M70 102C72 84 84 74 96 72" fill="none" stroke="#fff" stroke-opacity=".12" stroke-width="7" stroke-linecap="round"/>`);
    if (p.glow > .01 && !mini) g.push(`<ellipse cx="100" cy="${EY}" rx="58" ry="30" fill="${COL.cyan}" opacity="${n(.14 * p.glow)}"/>`);
    EX.forEach((x, i) => g.push(eye(x, i, p, mini, body)));
    if (!mini) {
      const both = state === 'celebrating' ? p.pawR : 0, wv = state === 'greeting' ? p.p1 : 0;
      const lx = 82 - 18 * both, ly = 170 - 42 * both;
      g.push(`<ellipse cx="${n(lx)}" cy="${n(ly)}" rx="11" ry="6" transform="rotate(${n(35 * both)} ${n(lx)} ${n(ly)})" fill="${COL.paw}" ${st}/>`);
      const px = 118 + (136 - 118) * p.pawR + 6 * wv, py = 170 + (128 - 170) * p.pawR - 2 * Math.abs(wv);
      g.push(`<ellipse cx="${n(px)}" cy="${n(py)}" rx="11" ry="6" transform="rotate(${n(-35 * p.pawR - 28 * wv)} ${n(px)} ${n(py)})" fill="${COL.paw}" ${st}/>`);
    }
    return g.join('');
  }

  const AMB = '#E6BE50', CY = '#82C8DC', CO = '#D77757', GR = '#78C878', PK = '#FF8FB1', PAPER = '#EEF0E6', LINE = '#3A3A46';
  const qm = (x, y, s, o) => `<g transform="translate(${n(x)} ${n(y)}) scale(${n(s)})" opacity="${n(o)}"><path d="M-5 -6a5.5 5.5 0 1 1 7 5.2c-2 .8-2 2-2 4" fill="none" stroke="${AMB}" stroke-width="3.2" stroke-linecap="round"/><circle cy="8.5" r="2" fill="${AMB}"/></g>`;
  function heldProps(state, p, ms) {
    const o = [];
    if (state === 'reading') {
      const line = Math.floor(p.p1 * 4) % 4;
      o.push(`<g transform="rotate(-4 100 150)"><path d="${rr(62, 126, 76, 48, 5)}" fill="${PAPER}" stroke="${LINE}" stroke-width="2"/>`);
      [0, 1, 2, 3].forEach(i => o.push(`<path d="M72 ${138 + i * 9}h${i === 3 ? 34 : 56}" stroke="${i === line ? CY : '#B8B8C4'}" stroke-width="${i === line ? 4 : 3}" stroke-linecap="round"/>`));
      o.push('</g>');
    }
    if (state === 'editing') {
      o.push(`<g transform="rotate(6 160 140)"><path d="${rr(138, 118, 48, 50, 4)}" fill="${PAPER}" stroke="${LINE}" stroke-width="2"/>`);
      const k = p.p1 * 3, row = Math.floor(k), fr = k - row;
      for (let i = 0; i < 3; i++) { const len = i < row ? 30 : i === row ? 30 * fr : 0; if (len > .5) o.push(`<path d="M146 ${132 + i * 11}q${n(len / 4)} -4 ${n(len / 2)} 0t${n(len / 2)} 0" fill="none" stroke="${LINE}" stroke-width="2.2" stroke-linecap="round"/>`); }
      const tx = 146 + 30 * fr, ty = 132 + 11 * Math.min(row, 2);
      o.push(`</g><g transform="rotate(6 160 140)"><path d="M${n(tx)} ${n(ty)}l4 -3 16 -22 -4 -3 -16 22z" fill="${CO}" stroke="${LINE}" stroke-width="1.5" stroke-linejoin="round"/><path d="M${n(tx + 20)} ${n(ty - 25)}l-4 -3 3 -4 4 3z" fill="${PK}"/></g>`);
    }
    if (state === 'searching') {
      const a = -0.75 + 1.5 * p.p1, cx = 100 + 46 * Math.sin(a), cy = 108 - 6 * Math.cos(a);
      o.push(`<path d="M${n(cx + 13)} ${n(cy + 13)}l18 18" stroke="${LINE}" stroke-width="7" stroke-linecap="round"/><path d="M${n(cx + 13)} ${n(cy + 13)}l18 18" stroke="${AMB}" stroke-width="4" stroke-linecap="round"/>`);
      o.push(`<circle cx="${n(cx)}" cy="${n(cy)}" r="18" fill="${CY}" fill-opacity=".22" stroke="${AMB}" stroke-width="5"/><path d="M${n(cx - 9)} ${n(cy - 6)}a10 10 0 0 1 8 -6" fill="none" stroke="#fff" stroke-opacity=".7" stroke-width="3" stroke-linecap="round"/>`);
    }
    if (state === 'building') {
      o.push(`<path d="${rr(144, 150, 40, 22, 3)}" fill="#2A2A33" stroke="${LINE}" stroke-width="2"/><path d="M152 158h8M166 164h10" stroke="${LINE}" stroke-width="2" stroke-linecap="round"/>`);
      const ang = -75 + 75 * p.p1;
      o.push(`<g transform="rotate(${n(ang)} 132 132)"><path d="M132 132L164 132" stroke="#8A6A46" stroke-width="5" stroke-linecap="round"/><path d="${rr(158, 120, 12, 24, 2)}" fill="#9696A0" stroke="${LINE}" stroke-width="2"/></g>`);
      if (p.p2 > 0 && p.p2 < 1) [[-1, -1], [1, -1.2], [1.4, -.3], [-1.4, -.4]].forEach(([dx, dy]) => o.push(`<path d="M${n(164 + dx * (4 + 8 * p.p2))} ${n(150 + dy * (4 + 8 * p.p2))}l${n(dx * 6)} ${n(dy * 6)}" stroke="${AMB}" stroke-width="3" stroke-linecap="round" opacity="${n(1 - p.p2)}"/>`));
    }
    if (state === 'testing') {
      o.push(`<path d="M152 104h20M156 104v22l-14 32c-2 6 2 12 8 12h24c6 0 10-6 8-12l-14-32v-22" fill="${PAPER}" fill-opacity=".9" stroke="${LINE}" stroke-width="2.5" stroke-linejoin="round"/>`);
      o.push(`<path d="M147 146h30l7 14c2 4 -1 8 -5 8h-34c-4 0 -7 -4 -5 -8z" fill="${CY}"/>`);
      for (let i = 0; i < 3; i++) { const ph = (p.p1 * 2 + i / 3) % 1; o.push(`<circle cx="${n(158 + i * 5 + 3 * Math.sin(ph * 9))}" cy="${n(160 - 56 * ph)}" r="${n(2.5 + i)}" fill="${CY}" opacity="${n(1 - ph)}"/>`); }
    }
    if (state === 'drinking') {
      const lvl = 1 - .55 * p.p1, k = p.p2, ang = -4 - 114 * k;
      o.push(`<g transform="translate(${n(162 - 4 * k)} ${n(162 - 54 * k)}) rotate(${n(ang)})"><path d="${rr(-10, -44, 20, 44, 6)}" fill="#DDEFF4" fill-opacity=".85" stroke="${LINE}" stroke-width="2"/>`);
      const wh = 38 * lvl;
      const wy = k > .5 ? -41 : -3 - wh;
      o.push(`<path d="${rr(-7, wy, 14, wh, 4)}" fill="${CY}"/><path d="M-5 -38v24" stroke="#fff" stroke-opacity=".7" stroke-width="2.5" stroke-linecap="round"/>`);
      o.push(`<path d="${rr(-7, -52, 14, 9, 2)}" fill="${CO}" stroke="${LINE}" stroke-width="2"/><path d="M-2 -52v-8h4v8" fill="#9696A0" stroke="${LINE}" stroke-width="1.5"/>`);
      if (p.p2 > .6) for (let i = 0; i < 3; i++) { const ph = (ms / 500 + i / 3) % 1; o.push(`<circle cx="${n(-2 + 3 * Math.sin(i * 2 + ph * 6))}" cy="${n(-40 + wh * ph)}" r="${n(1.6 + i * .5)}" fill="#fff" opacity="${n(.8 * (1 - ph))}"/>`); }
      o.push('</g>');
    }
    return o.join('');
  }
  function freeProps(state, p, ms) {
    const o = [];
    if (state === 'celebrating' && p.p1 > 0 && p.p1 < 1) {
      const cols = [CO, CY, AMB, GR, PK];
      for (let i = 0; i < 16; i++) { const a = -Math.PI * (.08 + .84 * (i / 15)), v = 70 + 30 * ((i * 37) % 7) / 7, t = p.p1;
        const x = 100 + Math.cos(a) * v * t, y = 80 + Math.sin(a) * v * t + 60 * t * t;
        o.push(`<rect x="${n(x - 3)}" y="${n(y - 5)}" width="6" height="10" rx="1.5" fill="${cols[i % 5]}" transform="rotate(${n(i * 47 + t * 540)} ${n(x)} ${n(y)})" opacity="${n(1 - t * t * t)}"/>`); }
    }
    if (state === 'confused') { o.push(qm(150, 50, p.p1, p.p1), qm(170, 30, p.p1 * .7, Math.max(0, p.p1 * 1.4 - .4))); }
    if (state === 'greeting') { const s = .7 + .4 * Math.abs(Math.sin(ms / 160)); o.push(`<path transform="translate(162 92) scale(${n(s * p.pawR)})" d="M0 -8L2 -2L8 0L2 2L0 8L-2 2L-8 0L-2 -2Z" fill="${AMB}"/>`); }
    if (state === 'poked' && p.p1 < 1) {
      const t = p.p1, a = 1 - t;
      [[-1, -.4], [-1, .3], [-.6, -1]].forEach(([dx, dy]) => o.push(`<path d="M${n(44 + dx * (4 + 14 * t))} ${n(108 + dy * (4 + 14 * t))}l${n(dx * 8)} ${n(dy * 8)}" stroke="${AMB}" stroke-width="3.5" stroke-linecap="round" opacity="${n(a)}"/>`));
      o.push(`<g transform="translate(160 ${n(64 - 10 * t)})" opacity="${n(Math.min(1, a * 2))}"><path d="M-2.5 -14h5l-1 14h-3z" fill="${AMB}"/><circle cy="6" r="3" fill="${AMB}"/></g>`);
    }
    if (state === 'eating') {
      o.push(`<g transform="translate(152 150)"><path d="M-22 -8L-30 -20L-14 -14Z M22 -8L30 -20L14 -14Z" fill="#B48A5E" stroke="${LINE}" stroke-width="2" stroke-linejoin="round"/>`);
      [[-10, -12, '#8A5A3E'], [-2, -15, '#A86E48'], [7, -12, '#8A5A3E'], [13, -15, '#A86E48'], [-14, -9, '#A86E48']].forEach(([x, y, c]) => o.push(`<circle cx="${x}" cy="${y}" r="3.4" fill="${c}"/>`));
      o.push(`<path d="M-22 -10H22L20 24H-20Z" fill="#C8A27A" stroke="${LINE}" stroke-width="2" stroke-linejoin="round"/><path d="M-22 -10H22" stroke="${LINE}" stroke-width="2"/>`);
      o.push(`<path d="M0 14c-6-4-9-7-9-10a4 4 0 0 1 9-1a4 4 0 0 1 9 1c0 3-3 6-9 10z" fill="${PK}"/></g>`);
      const ph = (p.p1 * 2) % 1;
      if (ph > .2 && ph < .7) { const k = (ph - .2) / .5; [[-1, 1], [1, 1.3], [0, 1.6]].forEach(([dx, s], i) => o.push(`<circle cx="${n(150 + dx * 12 * k)}" cy="${n(138 - 16 * s * k + 18 * k * k)}" r="2.2" fill="#A86E48" opacity="${n(1 - k)}"/>`)); }
    }
    if (state === 'playing') {
      const v = (((p.p1 - .12) % .5) + .5) % .5 / .5, bx = 160 + 10 * Math.sin(Math.PI * v), by = 162 - 34 * Math.sin(Math.PI * v), rot = p.p1 * 720;
      o.push(`<path d="M${n(bx)} ${n(by + 6)}C${n(bx + 6)} 176 184 172 196 176" fill="none" stroke="${PK}" stroke-width="2" stroke-linecap="round"/>`);
      o.push(`<ellipse cx="${n(bx)}" cy="175" rx="${n(11 - 5 * Math.sin(Math.PI * v))}" ry="3" fill="#000" opacity=".25"/>`);
      o.push(`<g transform="translate(${n(bx)} ${n(by)}) rotate(${n(rot)})"><circle r="11" fill="${PK}" stroke="${LINE}" stroke-width="2"/><path d="M-8 -6C-2 -2 2 2 8 7M-10 1C-4 2 2 5 5 10M-3 -10C1 -5 6 -2 10 -2" fill="none" stroke="#D9668E" stroke-width="1.8" stroke-linecap="round"/></g>`);
    }
    return o.join('');
  }
  function faceSvg(p, size, bg) {
    const tr = `translate(${n(100 + p.x * .4)} ${n(112 + p.y * .3)}) rotate(${n(p.rot * .8)}) translate(-100 -112)`, g = [];
    [[74, -p.earL, 1], [126, p.earR, -1]].forEach(([x, a, m]) => g.push(`<g transform="translate(${x} 80) rotate(${n(a)}) scale(${m} 1)"><path d="${EAR}" fill="${COL.mini}"/><path d="${EAR_IN}" fill="${COL.earIn}"/></g>`));
    if (p.glow > .01) g.push(`<ellipse cx="100" cy="${EY}" rx="58" ry="30" fill="${COL.cyan}" opacity="${n(.2 * p.glow)}"/>`);
    EX.forEach((x, i) => g.push(eye(x, i, p, false, bg || '#16131C')));
    return `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="40 28 120 120" style="display:block;overflow:visible"><g transform="${tr}" opacity="${n(1 - p.dim * .35)}">${g.join('')}</g></svg>`;
  }
  const ell = (cx, cy, rx, ry) => `M${n(cx - rx)} ${n(cy)}a${n(rx)} ${n(ry)} 0 1 0 ${n(2 * rx)} 0a${n(rx)} ${n(ry)} 0 1 0 ${n(-2 * rx)} 0Z`;
  function template(p, size, color) {
    const tr = `translate(100 172) rotate(${n(p.rot * .5)}) scale(${n(p.sx)} ${n(p.sy)}) translate(-100 -172)`;
    let holes = '';
    EX.forEach(x => {
      if (p.eye === 'closed' || p.eye === 'happy' || p.eyeOpen < .25) { holes += rr(x - 14, EY - 2.5, 28, 6, 3); return; }
      const ry = RX * 1.1 * Math.min(1, p.eyeOpen);
      holes += ell(x, EY, RX, ry) + ell(x + cl(p.lookX * 1.4, 7), EY + cl((p.lookY + BASE_LOOK_Y) * .6 + 2, ry * .35), RX * .5, Math.min(ry, RX) * .55);
    });
    const ears = [[74, -p.earL, 1], [126, p.earR, -1]].map(([x, a, m]) => `<path transform="translate(${x} 80) rotate(${n(a)}) scale(${m} 1)" d="${EAR}" fill="${color}"/>`).join('');
    return `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="30 30 140 146" style="display:block"><g transform="${tr}">${ears}<path d="${BODY}${holes}" fill="${color}" fill-rule="evenodd"/></g></svg>`;
  }

  function svg(state, t, opts) {
    opts = opts || {};
    const s = STATES[state] || STATES.idle;
    const u = ((t % 1) + 1) % 1, ms = u * s.dur;
    const p = poseX(state, u);
    const detail = opts.detail || 'full', mini = detail === 'mini', size = opts.size || 160;
    if (detail === 'template') return template(p, size, opts.color || '#000');
    if (detail === 'face') return faceSvg(p, size, opts.bg);
    const yk = mini ? .45 : 1;
    const tr = `translate(${n(100 + p.x * yk)} ${n(172 + p.y * yk)}) rotate(${n(p.rot)}) scale(${n(p.sx)} ${n(p.sy)}) translate(-100 -172)`;
    const lift = Math.max(0, -p.y);
    const sh = mini ? '' : `<ellipse cx="${n(100 + p.x)}" cy="176" rx="${n(48 * p.sx * (1 - lift / 70))}" ry="5" fill="#000" opacity="${n(.32 * (1 - lift / 50))}"/>`;
    const vb = mini ? '27 32 146 146' : '0 0 200 200';
    return `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="${vb}" style="display:block;overflow:visible">${sh}<g transform="${tr}" opacity="${n(1 - p.dim * .35)}">${figure(state, p, mini)}${mini ? '' : heldProps(state, p, ms)}</g>${mini || opts.noProps ? '' : ACT[state] ? freeProps(state, p, ms) : EC.props(state, p, ms)}</svg>`;
  }

  function spec(state) {
    const S0 = STATES[state];
    const b = ACT[state] ? { name: state, label: S0.label, durationMs: S0.dur, loop: true, fps: 60, viewBox: [0, 0, 200, 200], kind: 'action', tracks: Object.fromEntries(Object.entries(S0.tracks).map(([k, v]) => [k, v.map(([t, value, ease]) => ({ t, ms: Math.round(t * S0.dur), value, ease: ease || 'io' }))])) } : EC.spec(state);
    b.character = 'shade';
    b.pivot = { body: [100, 172], earL: [74, 80], earR: [126, 80], pawR: [118, 170] };
    b.parts = ['shadow', 'earL', 'earR', 'body', 'eyeL', 'eyeR', 'pupilL', 'pupilR', 'pawL', 'pawR', 'props'];
    b.palette = { body: COL.body, rim: COL.rim, earInner: COL.earIn, eye: COL.white, pupil: COL.pupil, effect: COL.cyan };
    b.notes = 'No mouth. Expression is eyes and ears only. Pupils follow lookX/lookY with a constant upward bias of 3.';
    return b;
  }

  window.ElisaShade = { STATES, ORDER: ALL, CORE: ORDER, ACTIONS, EASE, EASE_INFO, COL, pose: poseX, svg, spec };
})();
