(function () {
  const EC = window.ElisaChar;
  const { STATES, ORDER, pose, mix } = EC;
  const n = v => Math.round(v * 100) / 100;
  const rr = EC.rr;
  const dk = (c, t) => mix(c, '#000000', t == null ? .3 : t);
  const K = { coral: '#D77757', cyan: '#82C8DC', amber: '#E6BE50', green: '#78C878', ink: '#1E1E24', pink: '#F4C6D0', graphite: '#3A3A46', cream: '#ECE6DA', navy: '#2E3A55' };
  const KEY_T = { idle: .5, listening: .5, thinking: .6, streaming: .62, running: .6, permission: .5, success: .45, error: .5, interrupted: .3, memory: .4, sleeping: .35, waking: .85 };
  const P = (d, f, s) => `<path d="${d}" fill="${f}"${s ? ' ' + s : ''}/>`;
  const L = (d, c, w, o) => `<path d="${d}" fill="none" stroke="${c}" stroke-width="${w}" stroke-linecap="round" stroke-linejoin="round"${o != null ? ` opacity="${n(o)}"` : ''}/>`;
  const C = (x, y, r, f, s) => `<circle cx="${n(x)}" cy="${n(y)}" r="${n(r)}" fill="${f}"${s ? ' ' + s : ''}/>`;
  const E = (x, y, rx, ry, f, s) => `<ellipse cx="${n(x)}" cy="${n(y)}" rx="${n(rx)}" ry="${n(ry)}" fill="${f}"${s ? ' ' + s : ''}/>`;
  const ST = (c, w) => `stroke="${c}" stroke-width="${w || 2}" stroke-linejoin="round"`;
  const cl = (v, a) => Math.max(-a, Math.min(a, v));
  const fx = (v, T, p, mini) => typeof v === 'function' ? v(T, p, mini) : (v || '');
  const sin = (T, per, ph) => Math.sin(2 * Math.PI * (T / per + (ph || 0)));

  function eyeKit(x, y, R, iris, p, sd, ink) {
    const lx = cl(p.lookX * .45, R * .25), ly = cl(p.lookY * .4, R * .25);
    if (p.eye === 'happy') return L(`M${n(x - R)} ${n(y + 3)}Q${n(x)} ${n(y - R)} ${n(x + R)} ${n(y + 3)}`, ink, 3.6);
    if (p.eye === 'closed') return L(`M${n(x - R)} ${n(y)}Q${n(x)} ${n(y + R * .6)} ${n(x + R)} ${n(y)}`, ink, 3.2);
    const wide = p.eye === 'wide', sad = p.eye === 'sad', r = R * (wide ? 1.08 : 1);
    const bl = Math.max(.05, Math.min(1.15, p.eyeOpen)) * (sad ? .7 : 1);
    if (bl < .22) return L(`M${n(x - r)} ${n(y)}Q${n(x)} ${n(y + 4)} ${n(x + r)} ${n(y)}`, ink, 3);
    const ry = r * 1.12 * bl, ic = mix(iris, K.cyan, p.glow), oy = sad ? r * .25 : 0, cx = x + lx, cy = y + oy + ly;
    let g = E(x, y + oy, r, ry, K.ink) + E(cx, cy + r * .06, r * .84, ry * .86, ic) + E(cx, cy + ry * .42, r * .56, ry * .3, mix(ic, '#FFFFFF', .4), 'opacity=".8"') +
      E(cx, cy, r * (wide ? .34 : .46), ry * (wide ? .44 : .6), K.ink) + C(cx - r * .34, cy - ry * .36, r * (wide ? .34 : .3), '#fff') + C(cx + r * .3, cy + ry * .28, r * .13, '#fff', 'opacity=".9"');
    if (sad) g += L(`M${n(x - sd * r * .9)} ${n(y - r * 1.5)}L${n(x + sd * r * .9)} ${n(y - r * 1.15)}`, ink, 3);
    return g;
  }
  function eyeDot(x, y, R, p, col, ink, mini) {
    if (p.eye === 'happy') return L(`M${n(x - R * .8)} ${n(y + 3)}Q${n(x)} ${n(y - R * .8)} ${n(x + R * .8)} ${n(y + 3)}`, col, mini ? 4.6 : 3.4);
    const sad = p.eye === 'sad', wide = p.eye === 'wide';
    const bl = Math.max(.05, Math.min(1.15, p.eyeOpen)) * (sad ? .6 : 1);
    if (p.eye === 'closed' || bl < .22) return L(`M${n(x - R * .8)} ${n(y)}Q${n(x)} ${n(y + R * .5)} ${n(x + R * .8)} ${n(y)}`, col, mini ? 4.2 : 3);
    const rx = R * (wide ? .66 : .55), ry = R * (wide ? .8 : .7) * bl, c = mix(col, K.cyan, p.glow * .7);
    let g = E(x, y, rx, ry, c);
    if (!mini) g += C(x - R * .18, y - R * .25 * bl, R * (wide ? .24 : .2), /^#[0-3]/.test(col) ? '#fff' : K.ink, `opacity="${/^#[0-3]/.test(col) ? .9 : .5}"`);
    if (sad && !mini) g += L(`M${n(x - R)} ${n(y - R * 1.15)}L${n(x + R * .5)} ${n(y - R * .95)}`, ink, 2.6);
    return g;
  }
  function mouth(f, p, ms) {
    if (f.mouth === 'none') return '';
    const ink = f.ink || K.ink, x = f.x, y = f.y + (f.my || 16);
    if (f.mouth === 'beak') return P(`M${x - 6} ${y - 4}H${x + 6}L${x} ${y + 4}Z`, '#E8A040', ST(dk('#E8A040', .25), 1.5));
    let m = f.mouth || 'smile';
    if (p.eye === 'happy') m = 'open'; else if (p.eye === 'wide') m = 'o'; else if (p.eye === 'sad') m = 'frown'; else if (p.eye === 'closed') m = 'flat';
    if (p.talk && Math.floor(ms / 190) % 3 === 1) m = 'o';
    const d = {
      cat: L('M-6.3 -1q3.15 4 6.3 0q3.15 4 6.3 0', ink, 2.1),
      smile: L('M-5 -1Q0 3.5 5 -1', ink, 2.2),
      o: E(0, 1.5, 2.8, 3.4, '#8A3D3A', ST(ink, 1.2)),
      open: P('M-6 -2Q0 9 6 -2Z', '#8A3D3A', ST(ink, 1.5)) + L('M-3 3Q0 5 3 3', K.coral, 2),
      frown: L('M-5 3Q0 -2 5 3', ink, 2.1),
      flat: L('M-3.5 1h7', ink, 2.1)
    }[m] || '';
    return `<g transform="translate(${x} ${y})">${d}</g>`;
  }
  function face(f, p, ms, mini) {
    const g = [], ink = f.ink || K.ink;
    [-1, 1].forEach((sd, i) => {
      const x = f.x + sd * f.ex + p.lookX * .35, y = f.y + p.lookY * .35;
      const ir = Array.isArray(f.iris) ? f.iris[i] : f.iris;
      if (mini) g.push(eyeDot(x, y, Math.max(f.r * 1.05, 11), p, f.eyeCol || K.ink, ink, true));
      else if (f.eyes === 'kitten') g.push(eyeKit(x, y, f.r, ir || K.amber, p, sd, ink));
      else g.push(eyeDot(x, y, f.r, p, f.eyeCol || K.ink, ink, false));
    });
    if (!mini) {
      if (f.blush !== false) { const bo = p.eye === 'happy' ? .6 : .38, bx = f.ex + f.r * .8, by = f.y + f.r * .95; g.push(E(f.x - bx, by, 7, 3.6, K.coral, `opacity="${bo}"`), E(f.x + bx, by, 7, 3.6, K.coral, `opacity="${bo}"`)); }
      if (f.nose) g.push(P(`M${f.x - 4} ${f.y + 9}h8l-4 4.5z`, f.nose));
      g.push(mouth(f, p, ms));
    }
    return g.join('');
  }
  const EARS = {
    cat: (c, i, st) => P('M-14 8C-17 -14 -12 -32 -6 -34C0 -35 8 -20 14 2Z', c, st) + P('M-8 2C-10 -11 -8 -22 -5 -24C-1 -24 3 -14 7 0Z', i),
    tuft: (c, i, st) => P('M-14 8C-17 -14 -12 -32 -6 -34C0 -35 8 -20 14 2Z', c, st) + P('M-8 2C-10 -11 -8 -22 -5 -24C-1 -24 3 -14 7 0Z', i) + L('M-6 -33l-2 -12M-4 -33l3 -9', dk(c, .45), 2.5),
    fold: (c, i, st) => P('M-14 6C-16 -8 -8 -16 4 -14C12 -12 14 -4 12 4Z', c, st) + L('M-7 -6C-3 -10 5 -9 7 -4', dk(c, .25), 2.5),
    owl: (c, i, st) => P('M-12 10L-10 -20L12 6Z', c, st)
  };
  function ears(cfg, p, mini) {
    const e = cfg.ears; if (!e) return '';
    const col = e.col || cfg.col, st = mini ? '' : ST(cfg.rim, 2), shape = EARS[e.type](col, e.inner || K.coral, st);
    return [[100 - e.x, -p.earL, 1], [100 + e.x, p.earR, -1]].map(([x, a, m]) => `<g transform="translate(${x} ${e.y}) rotate(${n(a)}) scale(${m} 1)">${shape}</g>`).join('');
  }
  function tail(cfg, p, T) {
    if (!cfg.tail) return '';
    const w = 6 * sin(T, 2.6);
    return `<g transform="translate(146 158) rotate(${n(-p.tail - w)})">${L('M0 0C16 0 26 -10 26 -28C26 -38 31 -45 38 -44', cfg.rim, 12)}${L('M0 0C16 0 26 -10 26 -28C26 -38 31 -45 38 -44', cfg.tail, 8.5)}</g>`;
  }
  function paws(cfg, p) {
    if (!cfg.paws) return '';
    const st = ST(cfg.rim, 1.5), px = 118 + 18 * p.pawR, py = 170 - 42 * p.pawR;
    return E(82, 170, 11, 6, cfg.paws, st) + `<ellipse cx="${n(px)}" cy="${n(py)}" rx="11" ry="6" transform="rotate(${n(-35 * p.pawR)} ${n(px)} ${n(py)})" fill="${cfg.paws}" ${st}/>`;
  }

  function render(cfg, t, opts) {
    opts = opts || {};
    const state = opts.state && STATES[opts.state] ? opts.state : 'idle', S = STATES[state], size = opts.size || 160, small = size < 100, mini = !!opts.mini;
    const u = small ? KEY_T[state] : (((t * 1000 / S.dur) % 1) + 1) % 1, ms = u * S.dur, T = small ? 1.2 : t;
    const p = pose(state, u); p.talk = state === 'streaming';
    const fy = cfg.float && !small ? -5 - 4 * sin(T, 3, cfg.phase) : cfg.float ? -6 : 0;
    const yk = mini ? .45 : 1;
    const tr = `translate(${n(100 + p.x * yk)} ${n(172 + p.y * yk + fy)}) rotate(${n(p.rot)}) scale(${n(p.sx)} ${n(p.sy)}) translate(-100 -172)`;
    const st = mini ? '' : ST(cfg.rim, 2.2);
    const g = [];
    if (!mini) g.push(tail(cfg, p, T));
    g.push(fx(cfg.back, T, p, mini));
    g.push(ears(cfg, p, mini));
    g.push(typeof cfg.body === 'function' ? cfg.body(st, T, p) : P(cfg.body, cfg.col, st));
    g.push(fx(cfg.deco, T, p, mini));
    if (p.glow > .01 && !mini) g.push(E(cfg.face.x, cfg.face.y, cfg.face.ex + cfg.face.r * 2.2, cfg.face.r * 1.9, K.cyan, `opacity="${n(.16 * p.glow)}"`));
    g.push(face(cfg.face, p, ms, mini));
    g.push(fx(cfg.front, T, p, mini));
    if (!mini) g.push(paws(cfg, p));
    const lift = Math.max(0, -p.y - fy);
    const sh = mini ? '' : `<ellipse cx="${n(100 + p.x)}" cy="176" rx="${n(46 * p.sx * (1 - lift / 70))}" ry="5" fill="#000" opacity="${n(.3 * (1 - lift / 50))}"/>`;
    const props = mini || size < 100 || opts.noProps ? '' : EC.props(state, p, ms);
    return `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="${mini ? '24 26 152 152' : '0 0 200 200'}" style="display:block;overflow:visible">${sh}<g transform="${tr}" opacity="${n(1 - p.dim * .35)}">${g.join('')}</g>${props}</svg>`;
  }

  // ---------- Kitten eye ----------
  const KB = 'M50 150C44 112 66 80 100 80C134 80 156 112 150 150C148 166 132 172 100 172C68 172 52 166 50 150Z';
  const LOAF = 'M36 152C36 122 62 104 100 104C138 104 164 122 164 152C164 166 152 172 136 172H64C48 172 36 166 36 152Z';
  const kit = o => {
    const rim = o.rim || dk(o.col, .3), lf = !!o.loaf;
    return { col: o.col, rim, desc: o.desc, ears: { type: o.ear || 'cat', x: lf ? 34 : 30, y: lf ? 118 : 94, col: o.earCol, inner: o.inner }, tail: o.tail === false ? null : (o.tailCol || o.col), paws: o.pawCol || dk(o.col, .06),
      face: { x: 100, y: lf ? 136 : 124, ex: lf ? 21 : 22, r: o.r || 14, eyes: 'kitten', iris: o.iris, mouth: o.mouth || 'cat', my: 18, nose: o.nose || '#E8A08A', ink: o.ink }, body: lf ? LOAF : KB, deco: o.deco, front: o.front };
  };
  const collar = (c, bell) => L('M62 150Q100 163 138 150', c, 7) + (bell ? C(100, 160, 5, K.amber, ST(dk(K.amber, .3), 1.5)) : '');
  const KE = [
    ['KE1', 'Mochi', kit({ col: K.graphite, iris: K.amber, front: collar(K.coral, 1), desc: 'graphite · amber eyes · bell collar' })],
    ['KE2', 'Ginger', kit({ col: '#E3965C', iris: K.green, deco: L('M92 88l2 10M100 86v12M108 88l-2 10', '#B8683A', 3.5) + L('M52 128l11 2M51 140l11 0M148 128l-11 2M149 140l-11 0', '#B8683A', 3.5), desc: 'ginger tabby · green eyes' })],
    ['KE3', 'Snow', kit({ col: '#F4F2EE', rim: '#C9C3BA', inner: K.pink, iris: [K.cyan, K.amber], desc: 'white · odd eyes' })],
    ['KE4', 'Tux', kit({ col: '#1C1C22', rim: '#4A4A58', inner: K.coral, iris: K.cyan, pawCol: '#F4F2EE', deco: P('M80 172C78 152 88 142 100 142C112 142 122 152 120 172Z', '#F4F2EE') + E(100, 140, 15, 9, '#F4F2EE'), nose: '#E8A08A', ink: K.ink, desc: 'tuxedo · cyan eyes · white bib' })],
    ['KE5', 'Calico', kit({ col: '#F2EAD8', rim: '#C9BFA8', iris: K.amber, deco: P('M62 98C72 86 90 86 94 98C90 110 68 112 62 98Z', '#E3965C') + P('M118 92C132 86 146 96 146 110C134 116 122 108 118 92Z', K.graphite) + P('M56 150C60 140 74 140 78 150C74 160 60 160 56 150Z', '#E3965C'), desc: 'calico patches · amber eyes' })],
    ['KE6', 'Siamese', kit({ col: '#EFE4D2', rim: '#C9BCA4', earCol: '#5A4638', inner: '#8A6A56', iris: '#6FB4E6', pawCol: '#5A4638', tailCol: '#5A4638', deco: E(100, 140, 22, 15, '#5A4638', 'opacity=".85"'), nose: '#2E2420', desc: 'colour points · blue eyes' })],
    ['KE7', 'Fold', kit({ col: '#9A9AA6', ear: 'fold', iris: K.amber, r: 16, mouth: 'o', desc: 'folded ears · round amber eyes' })],
    ['KE8', 'Lilac', kit({ col: '#B8AECB', rim: '#8E83A3', iris: K.coral, front: `<g transform="translate(128 90) rotate(14)">${P('M0 0L-12 -7L-11 7Z M0 0L12 -7L11 7Z', K.coral, ST(dk(K.coral), 1.4))}${C(0, 0, 3.5, dk(K.coral, .15))}</g>`, desc: 'lilac · coral eyes · bow' })],
    ['KE9', 'Lynx kit', kit({ col: '#C8A27A', rim: '#9A7A56', ear: 'tuft', iris: K.green, deco: P('M52 132L38 146L52 148L44 160L60 156Z', '#C8A27A', ST('#9A7A56', 2)) + P('M148 132L162 146L148 148L156 160L140 156Z', '#C8A27A', ST('#9A7A56', 2)) + C(70, 104, 2.5, '#8A6A46') + C(130, 104, 2.5, '#8A6A46') + C(76, 96, 2, '#8A6A46') + C(124, 96, 2, '#8A6A46'), desc: 'ear tufts · cheek ruff · green eyes' })],
    ['KE10', 'Loaf', kit({ col: '#4A4A58', loaf: 1, tail: false, iris: K.amber, front: L('M136 170C156 170 162 150 150 140', '#36363F', 12) + L('M136 170C156 170 162 150 150 140', '#4A4A58', 8.5), desc: 'loaf pose · tail tucked · amber eyes' })]
  ];

  // ---------- Mind ----------
  const CLOUD = 'M52 168C34 168 30 146 44 138C36 118 56 104 70 112C72 90 100 82 112 98C126 84 152 96 146 118C166 120 170 148 152 156C156 168 146 172 136 168Z';
  const MI = [
    ['MI1', 'Brainy', { col: '#C9B8E0', rim: '#9C88B8', desc: 'brain body · folds · dot eyes', body: 'M48 140C40 118 52 96 70 94C74 80 92 74 104 82C118 72 140 80 142 96C160 100 166 124 154 140C160 158 144 172 124 170H76C56 172 42 158 48 140Z',
      deco: L('M70 108C80 104 86 112 96 108M110 100C120 96 128 104 136 100M58 126C66 122 72 128 80 124M122 124C130 120 138 126 148 122M100 86V102', '#A892C4', 3), face: { x: 100, y: 138, ex: 20, r: 12, mouth: 'smile', my: 15 } }],
    ['MI2', 'Thought', { col: K.cream, rim: '#BDB3A0', float: 1, phase: .2, desc: 'thought cloud · trailing bubbles', body: CLOUD,
      back: (T, p, mini) => mini ? '' : C(42, n(98 + 3 * sin(T, 2)), 9, K.cream, ST('#BDB3A0', 2)) + C(28, n(78 + 3 * sin(T, 2, .3)), 5.5, K.cream, ST('#BDB3A0', 2)), face: { x: 100, y: 134, ex: 20, r: 12, mouth: 'smile', my: 15 } }],
    ['MI3', 'Bulb', { col: '#F6E3A0', rim: '#C9A94E', desc: 'lightbulb · glowing filament', body: 'M100 54C130 54 150 76 150 104C150 124 136 134 128 146V156H72V146C64 134 50 124 50 104C50 76 70 54 100 54Z',
      back: (T, p, mini) => mini ? '' : C(100, 102, n(62 + 3 * sin(T, 1.6)), K.amber, `opacity="${n(.14 + .06 * sin(T, 1.6))}"`),
      deco: (T) => L('M90 146V130L95 124L100 130L105 124L110 130V146', K.coral, 3, .7 + .3 * sin(T, 1.6)) + `<path d="${rr(72, 154, 56, 18, 5)}" fill="${K.graphite}" stroke="#50505F" stroke-width="2"/>` + L('M74 160h52M74 166h52', '#50505F', 2), face: { x: 100, y: 94, ex: 20, r: 12, mouth: 'smile', my: 15 } }],
    ['MI4', 'Neuron', { col: K.graphite, rim: '#50505F', desc: 'dendrites · pulsing cyan tips', body: 'M54 124a46 46 0 1 0 92 0a46 46 0 1 0-92 0Z',
      back: (T, p, mini) => L('M70 90L54 66L40 62M54 66L52 48M130 90L146 66L160 62M146 66L148 48M58 152L34 162M142 152L166 162', '#50505F', 5) + [[40, 62], [52, 48], [160, 62], [148, 48], [34, 162], [166, 162]].map(([x, y], i) => C(x, y, 5, K.cyan, `opacity="${n(mini ? 1 : .45 + .55 * Math.max(0, sin(T, 1.8, i / 6)))}"`)).join(''),
      face: { x: 100, y: 124, ex: 18, r: 13, eyeCol: '#E8E8E8', ink: '#E8E8E8', mouth: 'smile', my: 16, blush: false } }],
    ['MI5', 'Orb', { col: '#BFE3EE', rim: K.cyan, desc: 'crystal ball · swirling inside', body: st => C(100, 106, 46, '#BFE3EE', st) + P('M68 172C68 158 82 150 100 150C118 150 132 158 132 172Z', K.graphite, st ? ST('#50505F', 2) : ''),
      deco: (T, p, mini) => mini ? '' : `<g transform="rotate(${n(T * 40)} 100 106)">${L('M80 118C76 100 92 88 106 94C120 100 116 118 102 118', '#FFFFFF', 4, .55)}</g>` + `<ellipse cx="78" cy="84" rx="12" ry="7" transform="rotate(-30 78 84)" fill="#fff" opacity=".7"/>`, face: { x: 100, y: 108, ex: 18, r: 12, mouth: 'smile', my: 15 } }],
    ['MI6', 'Prism', { col: '#B8AECB', rim: '#8E83A3', desc: 'gem prism · light split', body: 'M100 60L156 160C158 166 154 172 148 172H52C46 172 42 166 44 160Z',
      back: (T, p, mini) => mini ? '' : L('M8 128L72 128', '#FFFFFF', 3, .5 + .3 * sin(T, 2)) + L('M132 124L192 108', K.coral, 3, .8) + L('M134 130L194 126', K.amber, 3, .8) + L('M134 136L192 146', K.cyan, 3, .8),
      deco: L('M100 62L74 170M100 62L126 170', '#A196B8', 2), face: { x: 100, y: 138, ex: 18, r: 11, mouth: 'smile', my: 15 } }],
    ['MI7', 'Jelly', { col: '#D9B8E0', rim: '#A888B8', float: 1, phase: .5, desc: 'jellyfish · swaying tendrils', body: 'M44 120C44 86 70 64 100 64C130 64 156 86 156 120C150 126 140 120 132 126C124 132 112 124 100 128C88 124 76 132 68 126C60 120 50 126 44 120Z',
      back: (T, p, mini) => [66, 88, 112, 134].map((x, i) => { const w = mini ? 0 : 6 * sin(T, 2.2, i * .2); return L(`M${x} 124C${n(x + w)} 138 ${n(x - w)} 152 ${n(x + w * .6)} 168`, '#C49AD0', 5); }).join(''), face: { x: 100, y: 100, ex: 20, r: 12, mouth: 'smile', my: 15 } }],
    ['MI8', 'Sage', { col: '#5A6A84', rim: '#46546A', desc: 'owl · round glasses · beak', ears: { type: 'owl', x: 30, y: 82 }, body: 'M100 62C134 62 154 100 154 132C154 158 130 172 100 172C70 172 46 158 46 132C46 100 66 62 100 62Z',
      deco: E(100, 148, 30, 20, '#C9C9D4'), front: (T, p, mini) => mini ? '' : C(78 + p.lookX * .35, 112 + p.lookY * .35, 19, 'none', ST(K.amber, 3)) + C(122 + p.lookX * .35, 112 + p.lookY * .35, 19, 'none', ST(K.amber, 3)) + L(`M${n(97 + p.lookX * .35)} ${n(110 + p.lookY * .35)}h6`, K.amber, 3),
      face: { x: 100, y: 112, ex: 22, r: 14, eyes: 'kitten', iris: K.amber, mouth: 'beak', my: 18, blush: false } }],
    ['MI9', 'Monk', { col: K.cream, rim: '#BDB3A0', float: 1, phase: .7, desc: 'levitating · third eye', body: st => P('M40 160C40 132 66 116 100 116C134 116 160 132 160 160C160 168 152 172 140 172H60C48 172 40 168 40 160Z', K.cream, st) + C(100, 100, 34, K.cream, st),
      deco: (T, p, mini) => L('M68 134Q100 152 132 134', K.coral, 5) + `<path transform="translate(100 78) scale(${n(mini ? 1 : .9 + .15 * sin(T, 2.4))})" d="M0 -6L5 0L0 6L-5 0Z" fill="${K.coral}"/>`, face: { x: 100, y: 104, ex: 15, r: 10, mouth: 'smile', my: 13 } }],
    ['MI10', 'Spark', { col: '#4A4A58', rim: '#36363F', float: 1, phase: .9, desc: 'storm cloud · idea spark', body: CLOUD,
      back: (T, p, mini) => P('M104 34L90 58H102L94 80L120 50H106L114 34Z', K.amber, `opacity="${mini ? 1 : n(Math.floor(T * 3) % 4 ? 1 : .35)}"`), face: { x: 100, y: 134, ex: 20, r: 12, eyeCol: '#E8E8E8', ink: '#E8E8E8', mouth: 'smile', my: 15, blush: false } }]
  ];

  // ---------- Solar ----------
  const rays = (cx, cy, r0, r1, nR, col, rot, w) => `<g transform="rotate(${n(rot)} ${cx} ${cy})">${Array.from({ length: nR }, (_, i) => { const a = i / nR * Math.PI * 2, b = w / r0; return P(`M${n(cx + Math.cos(a - b) * r0)} ${n(cy + Math.sin(a - b) * r0)}L${n(cx + Math.cos(a) * r1)} ${n(cy + Math.sin(a) * r1)}L${n(cx + Math.cos(a + b) * r0)} ${n(cy + Math.sin(a + b) * r0)}Z`, col); }).join('')}</g>`;
  const SO = [
    ['SO1', 'Sunny', { col: '#F2C95C', rim: '#D9A83C', desc: 'sun · turning rays', body: 'M52 120a48 48 0 1 0 96 0a48 48 0 1 0-96 0Z', back: (T, p, mini) => rays(100, 120, 50, 70, 12, '#E89A5C', mini ? 0 : T * 18, 8), face: { x: 100, y: 120, ex: 18, r: 12, mouth: 'smile', my: 15 } }],
    ['SO2', 'Sunflower', { col: '#B07A52', rim: '#8A5A3E', desc: 'sunflower · petals · leaves', body: st => C(100, 104, 30, '#B07A52', st),
      back: (T, p, mini) => L('M100 172V134', '#5E9A4E', 6) + P('M100 156C86 150 74 154 70 162C82 166 94 164 100 156Z', K.green) + P('M100 148C114 142 126 146 130 154C118 158 106 156 100 148Z', K.green) + `<g transform="rotate(${n(mini ? 0 : 3 * sin(T, 3))} 100 104)">${Array.from({ length: 12 }, (_, i) => `<ellipse cx="100" cy="64" rx="9" ry="18" transform="rotate(${i * 30} 100 104)" fill="#F2C95C" stroke="#D9A83C" stroke-width="1.5"/>`).join('')}</g>`,
      face: { x: 100, y: 104, ex: 12, r: 9, mouth: 'smile', my: 12, blush: false } }],
    ['SO3', 'Panel bot', { col: K.graphite, rim: '#50505F', desc: 'solar panel · cyan cell grid', body: st => `<path d="${rr(56, 100, 88, 64, 16)}" fill="${K.graphite}" ${st}/>` + `<path d="${rr(70, 162, 14, 10, 3)}" fill="#2A2A33"/><path d="${rr(116, 162, 14, 10, 3)}" fill="#2A2A33"/>`,
      back: (T, p, mini) => L('M100 100V84', '#50505F', 4) + `<g transform="rotate(${n(-10 + (mini ? 0 : 3 * sin(T, 4)))} 100 74)"><path d="${rr(50, 60, 100, 28, 4)}" fill="${K.navy}" stroke="${K.cyan}" stroke-width="2"/>${mini ? '' : L('M75 60V88M100 60V88M125 60V88M50 74H150', K.cyan, 1.2, .6)}</g>`,
      face: { x: 100, y: 128, ex: 18, r: 11, eyeCol: K.cyan, ink: '#E8E8E8', mouth: 'smile', my: 15, blush: false } }],
    ['SO4', 'Sunrise', { col: '#F2C95C', rim: '#D9A83C', desc: 'half sun · horizon waves', body: 'M44 152A56 56 0 0 1 156 152Z',
      back: (T, p, mini) => `<g opacity="${n(mini ? 1 : .7 + .3 * sin(T, 3))}">${Array.from({ length: 7 }, (_, i) => { const a = Math.PI * (1.08 + i * .14), b = .09; return P(`M${n(100 + Math.cos(a - b) * 62)} ${n(152 + Math.sin(a - b) * 62)}L${n(100 + Math.cos(a) * 82)} ${n(152 + Math.sin(a) * 82)}L${n(100 + Math.cos(a + b) * 62)} ${n(152 + Math.sin(a + b) * 62)}Z`, '#E89A5C'); }).join('')}</g>`,
      front: (T, p, mini) => P(`M24 152Q${n(40 + (mini ? 0 : 4 * sin(T, 2)))} 146 56 152T88 152T120 152T152 152T184 152V176H24Z`, '#5FA8BE'), face: { x: 100, y: 126, ex: 20, r: 12, mouth: 'smile', my: 14 } }],
    ['SO5', 'Eclipse', { col: '#1C1C22', rim: '#3A3A46', desc: 'eclipse · glowing corona', body: 'M52 118a48 48 0 1 0 96 0a48 48 0 1 0-96 0Z',
      back: (T, p, mini) => C(100, 118, n(mini ? 58 : 58 + 3 * sin(T, 2.4)), K.amber, `opacity="${n(.3 + (mini ? 0 : .1 * sin(T, 2.4)))}"`) + C(100, 118, 52, 'none', ST(K.amber, 4)), face: { x: 100, y: 118, ex: 18, r: 12, eyeCol: '#F6E3A0', ink: '#F6E3A0', mouth: 'smile', my: 15, blush: false } }],
    ['SO6', 'Comet', { col: '#E4F2F6', rim: '#9CCAD8', float: 1, phase: .3, desc: 'comet · streaming tail', body: 'M72 124a40 40 0 1 0 80 0a40 40 0 1 0-80 0Z',
      back: (T, p, mini) => { const w = mini ? 0 : 3 * sin(T, 1.4); return P(`M86 98C60 ${n(84 + w)} 34 ${n(62 + w)} 18 40C46 ${n(64 - w)} 64 72 82 86Z`, K.cyan, 'opacity=".55"') + P(`M76 118C48 ${n(112 - w)} 26 ${n(100 - w)} 10 88C38 ${n(104 + w)} 56 110 74 112Z`, K.cyan, 'opacity=".4"') + P(`M80 140C56 ${n(142 + w)} 36 ${n(138 + w)} 20 132C42 ${n(146 - w)} 60 150 80 148Z`, K.cyan, 'opacity=".3"'); },
      face: { x: 112, y: 124, ex: 15, r: 11, mouth: 'smile', my: 14 } }],
    ['SO7', 'Ringed planet', { col: '#E8A06A', rim: '#C47E4C', desc: 'planet · bands · tilted ring', body: 'M56 118a44 44 0 1 0 88 0a44 44 0 1 0-88 0Z',
      back: L('M36 130C30 108 170 82 166 104', K.cyan, 7), deco: L('M60 100Q100 108 140 98M58 138Q100 146 142 136', '#D98A54', 4), front: L('M36 130C42 152 170 126 166 104', K.cyan, 7), face: { x: 100, y: 118, ex: 17, r: 11, mouth: 'smile', my: 14 } }],
    ['SO8', 'Flare', { col: '#E8785A', rim: '#C45A40', desc: 'solar flare · looping prominences', body: 'M54 124a46 46 0 1 0 92 0a46 46 0 1 0-92 0Z',
      back: (T, p, mini) => { const s = mini ? 1 : 1 + .08 * sin(T, 1.8); return `<g transform="translate(100 124) scale(${n(s)}) translate(-100 -124)">${L('M66 98C54 64 86 54 90 82', '#F2A65A', 7)}${L('M118 84C122 58 152 62 138 100', '#F2A65A', 7)}</g>`; },
      deco: (T, p, mini) => C(100, 124, 30, K.amber, `opacity="${n(mini ? .3 : .25 + .1 * sin(T, 1.8))}"`), face: { x: 100, y: 124, ex: 18, r: 12, mouth: 'smile', my: 15 } }],
    ['SO9', 'Dawn chick', { col: '#F6D35C', rim: '#D9AE3C', desc: 'chick · sun halo · beak', body: 'M100 72C132 72 152 102 152 134C152 158 128 172 100 172C72 172 48 158 48 134C48 102 68 72 100 72Z',
      back: (T, p, mini) => C(100, 104, 58, 'none', ST(K.amber, 3.5)) + rays(100, 104, 62, 72, 16, K.amber, mini ? 0 : -T * 10, 3), deco: E(54, 136, 9, 16, '#E8C04A', 'transform="rotate(20 54 136)"') + E(146, 136, 9, 16, '#E8C04A', 'transform="rotate(-20 146 136)"') + L('M100 74C96 62 104 58 108 64', '#D9AE3C', 4),
      front: (T, p, mini) => mini ? '' : E(88, 173, 8, 3.5, '#E8A040') + E(112, 173, 8, 3.5, '#E8A040'), face: { x: 100, y: 116, ex: 20, r: 12, mouth: 'beak', my: 15 } }],
    ['SO10', 'Sun jar', { col: '#DDEFF4', rim: '#9CCAD8', desc: 'glass jar · captured sun', body: 'M62 86H138V96C150 104 154 118 154 136V156C154 166 146 172 136 172H64C54 172 46 166 46 156V136C46 118 50 104 62 96Z',
      deco: (T, p, mini) => C(100, 148, n(mini ? 18 : 17 + 2 * sin(T, 1.6)), K.amber, `opacity="${n(mini ? .5 : .3 + .1 * sin(T, 1.6))}"`) + C(100, 148, 12, '#F2C95C') + `<path d="${rr(56, 72, 88, 16, 5)}" fill="${K.coral}" stroke="${dk(K.coral)}" stroke-width="2"/>` + (mini ? '' : L('M62 104C58 116 58 128 60 140', '#fff', 4, .6)),
      face: { x: 100, y: 112, ex: 20, r: 11, mouth: 'smile', my: 15 } }]
  ];

  const FAMILIES = { KE: 'Kitten eye', MI: 'Mind', SO: 'Solar' };
  const OPTIONS = [...KE, ...MI, ...SO].map(([id, name, cfg], i) => ({ id, name, fk: id.replace(/\d+$/, ''), family: FAMILIES[id.replace(/\d+$/, '')], cfg: Object.assign({ phase: (i * .173) % 1 }, cfg) }));
  const STATE_ROW = [['thinking', 'think'], ['success', 'done'], ['error', 'error'], ['sleeping', 'sleep']];
  window.ElisaExtra = { OPTIONS, FAMILIES, render, ORDER, STATES, STATE_ROW };
})();
