(function () {
  const n = v => Math.round(v * 100) / 100;
  const rr = (x, y, w, h, r) => { r = Math.min(r, w / 2, h / 2); return `M${n(x + r)} ${n(y)}H${n(x + w - r)}A${r} ${r} 0 0 1 ${n(x + w)} ${n(y + r)}V${n(y + h - r)}A${r} ${r} 0 0 1 ${n(x + w - r)} ${n(y + h)}H${n(x + r)}A${r} ${r} 0 0 1 ${n(x)} ${n(y + h - r)}V${n(y + r)}A${r} ${r} 0 0 1 ${n(x + r)} ${n(y)}Z`; };
  const mix = (a, b, t) => { const pa = parseInt(a.slice(1), 16), pb = parseInt(b.slice(1), 16); return '#' + [16, 8, 0].map(s => Math.round(((pa >> s) & 255) * (1 - t) + ((pb >> s) & 255) * t).toString(16).padStart(2, '0')).join(''); };
  const dk = (c, t) => mix(c, '#000000', t == null ? .32 : t);
  const K = { coral: '#D77757', cyan: '#82C8DC', green: '#78C878', amber: '#E6BE50', ink: '#1E1E24', white: '#F4F4F7', steel: '#6A7A9A' };
  const HAIR = { graphite: '#2E2E38', silver: '#C9C9D4', coral: '#D77757', cyan: '#82C8DC', ivory: '#ECE6DA', navy: '#2E3A55', amber: '#E6BE50', steel: '#5A6A84' };
  const SKIN = { light: '#F4DDCC', medium: '#D9A887', deep: '#9A6448' };
  const IRIS = { coral: K.coral, cyan: K.cyan, amber: K.amber, green: K.green, steel: K.steel };

  function eye(x, y, type, iris, bl, skin, side, s) {
    s = s || 1;
    const g = [], ink = K.ink, out = side < 0 ? -1 : 1;
    const T = c => `<g transform="translate(${n(x)} ${n(y)}) scale(${s})">${c}</g>`;
    if (type === 'happy') return T(`<path d="M-10 3Q0 -9 10 3" fill="none" stroke="${ink}" stroke-width="3.6" stroke-linecap="round"/>`);
    if (bl < .25) return T(`<path d="M-11 0Q0 5 11 0" fill="none" stroke="${ink}" stroke-width="3.2" stroke-linecap="round"/>`);
    const irisL = type === 'hetero' ? (side < 0 ? K.coral : K.cyan) : iris;
    g.push(`<ellipse cx="0" cy="0" rx="11" ry="${n(13.5 * bl)}" fill="#FFFFFF"/>`);
    g.push(`<ellipse cx="0" cy="${n(1.2 * bl)}" rx="9" ry="${n(12 * bl)}" fill="${irisL}"/>`);
    g.push(`<ellipse cx="0" cy="${n(-5 * bl)}" rx="9" ry="${n(6 * bl)}" fill="${ink}" opacity=".2"/>`);
    if (type === 'cursor') g.push(`<path d="${rr(-2.6, -4 * bl, 5.2, 10 * bl, 1.2)}" fill="${ink}"/>`);
    else g.push(`<ellipse cx="0" cy="${n(1.5 * bl)}" rx="4" ry="${n(5.5 * bl)}" fill="${ink}"/>`);
    if (type === 'starry') g.push(`<path transform="translate(-3.5 ${n(-4.5 * bl)})" d="M0 -4.5L1.2 -1.2L4.5 0L1.2 1.2L0 4.5L-1.2 1.2L-4.5 0L-1.2 -1.2Z" fill="#fff"/>`);
    else g.push(`<circle cx="-3.5" cy="${n(-5 * bl)}" r="3.2" fill="#fff"/>`);
    g.push(`<circle cx="3.6" cy="${n(5 * bl)}" r="1.6" fill="#fff" opacity=".9"/>`);
    if (type === 'calm') g.push(`<path d="M-13 ${n(-3 * bl)}Q0 ${n(-9 * bl)} 13 ${n(-3 * bl)}L13 -18H-13Z" fill="${skin}"/><path d="M-12.5 ${n(-3 * bl)}Q0 ${n(-9.5 * bl)} 12.5 ${n(-3.5 * bl)}" fill="none" stroke="${ink}" stroke-width="3.4" stroke-linecap="round"/>`);
    else g.push(`<path d="M-12 ${n(-7 * bl)}Q-1 ${n(-21 * bl)} 12.5 ${n(-8.5 * bl)}" fill="none" stroke="${ink}" stroke-width="3.2" stroke-linecap="round"/>`);
    const ox = -12 * out;
    if (type === 'tsurime') g.push(`<path d="M${ox} ${n(-7 * bl)}l${-4 * out} -4" stroke="${ink}" stroke-width="3" stroke-linecap="round"/>`);
    if (type === 'tareme') g.push(`<path d="M${ox} ${n(-7 * bl)}l${-3 * out} 4" stroke="${ink}" stroke-width="3" stroke-linecap="round"/>`);
    if (type === 'sparkle' || type === 'starry' || type === 'hetero') g.push(`<path d="M${ox} ${n(-7 * bl)}l${-3.5 * out} .5" stroke="${ink}" stroke-width="2.4" stroke-linecap="round"/>`);
    return T(g.join(''));
  }
  function mouth(type, x, y, s) {
    s = s || 1; const ink = K.ink;
    const m = { smile: `<path d="M-5 -1Q0 3.5 5 -1" fill="none" stroke="${ink}" stroke-width="2.2" stroke-linecap="round"/>`,
      cat: `<path d="M-7 -1q3.5 4.5 7 0q3.5 4.5 7 0" fill="none" stroke="${ink}" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/>`,
      o: `<ellipse cx="0" cy="1" rx="2.8" ry="3.4" fill="#7A3A36"/>`,
      open: `<path d="M-6 -2Q0 9 6 -2Z" fill="#8A3D3A" stroke="${ink}" stroke-width="1.6" stroke-linejoin="round"/><path d="M-3 3Q0 5 3 3" stroke="${K.coral}" stroke-width="2" stroke-linecap="round"/>`,
      flat: `<path d="M-4 0h8" stroke="${ink}" stroke-width="2.2" stroke-linecap="round"/>`,
      fang: `<path d="M-6 -1Q0 4 6 -1" fill="none" stroke="${ink}" stroke-width="2.2" stroke-linecap="round"/><path d="M2 0.6l1.6 3.4 1.4-3.8Z" fill="#fff" stroke="${ink}" stroke-width=".8"/>` }[type] || '';
    return `<g transform="translate(${n(x)} ${n(y)}) scale(${s})">${m}</g>`;
  }
  const blush = (x1, x2, y) => `<ellipse cx="${x1}" cy="${y}" rx="8" ry="4" fill="${K.coral}" opacity=".42"/><ellipse cx="${x2}" cy="${y}" rx="8" ry="4" fill="${K.coral}" opacity=".42"/>`;

  // ---------- human chibi ----------
  const BACK = {
    bob: 'M46 92C42 50 70 34 100 34C130 34 158 50 154 92C156 110 152 124 144 132L130 128H70L56 132C48 124 44 110 46 92Z',
    long: 'M44 92C40 48 70 32 100 32C130 32 160 48 156 92C160 130 158 160 150 176H50C42 160 40 130 44 92Z',
    wavy: 'M44 92C40 48 70 32 100 32C130 32 160 48 156 92C164 112 152 124 160 142C166 156 152 166 154 176H46C48 166 34 156 40 142C48 124 36 112 44 92Z',
    short: 'M50 92C46 52 72 36 100 36C128 36 154 52 150 92C150 102 148 110 146 114H54C52 110 50 102 50 92Z',
    spiky: 'M48 98L42 72L55 66L52 46L69 49L73 30L89 40L100 22L111 40L127 30L131 49L148 46L145 66L158 72L152 98Z',
    messy: 'M48 98L40 84L50 80C46 50 72 34 100 34C128 34 154 50 150 80L160 86L150 98Z'
  };
  const BANGS = {
    jagged: 'M46 90C44 50 70 30 100 30C130 30 156 50 154 90L146 76L138 92L126 70L114 90L100 68L88 90L76 70L64 92L56 76Z',
    straight: 'M46 92C44 48 70 30 100 30C130 30 156 48 154 92L148 78H52Z',
    parted: 'M46 92C44 48 70 30 100 30C130 30 156 48 154 92L146 74C132 62 112 58 102 46C92 60 70 62 56 74Z',
    swept: 'M46 92C44 48 70 30 100 30C130 30 156 48 154 86L142 72C120 76 96 70 70 58L66 80L56 74Z',
    spiky: 'M46 92C42 52 70 32 100 32C130 32 158 52 154 92L148 74L140 90L132 68L118 86L108 64L96 86L84 64L72 88L62 70L54 84Z'
  };
  const LOCK = 'M46 88C44 104 48 120 56 134C58 118 59 100 58 86Z';
  const TAIL = 'M0 0C-14 10 -22 40 -16 70C-12 86 -4 92 2 88C-4 70 -2 30 8 8Z';

  function outfit(type, c, o) {
    const st = `stroke="${dk(c)}" stroke-width="1.6" stroke-linejoin="round"`;
    const base = 'M70 132Q64 152 60 176H140Q136 152 130 132Z';
    switch (type) {
      case 'cloak': return `<path d="M54 176L70 130H130L146 176Z" fill="${c}" ${st}/><circle cx="100" cy="136" r="5" fill="${K.coral}"/>`;
      case 'sailor': return `<path d="${base}" fill="${K.white}" stroke="#BDBDC6" stroke-width="1.6"/><path d="M70 132L100 156L130 132L134 146L100 166L66 146Z" fill="${HAIR.navy}"/><path d="M71 140L100 160L129 140" fill="none" stroke="${K.white}" stroke-width="1.8"/><path d="M92 152l8 6 8-6-8 10z" fill="${K.coral}"/>`;
      case 'coat': return `<path d="${base}" fill="${K.white}" stroke="#BDBDC6" stroke-width="1.6"/><path d="M86 132L100 158L114 132M100 158V176" fill="none" stroke="#BDBDC6" stroke-width="1.8"/><path d="${rr(116, 150, 4, 12, 1.5)}" fill="${K.coral}"/>`;
      case 'kimono': return `<path d="${base}" fill="${c}" ${st}/><path d="M84 132L104 162M116 132L96 162" stroke="${K.white}" stroke-width="4" stroke-linecap="round"/><path d="${rr(64, 154, 72, 11, 3)}" fill="${K.coral}"/>`;
      case 'blazer': return `<path d="${base}" fill="${HAIR.navy}" stroke="${dk(HAIR.navy)}" stroke-width="1.6"/><path d="M88 132L100 152L112 132Z" fill="${K.white}"/><path d="M100 136l-3 4 3 16 3-16z" fill="${K.coral}"/>`;
      case 'cardigan': return `<path d="${base}" fill="${c}" ${st}/><path d="M88 132L100 156L112 132Z" fill="${K.white}"/><circle cx="100" cy="162" r="1.8" fill="${dk(c, .5)}"/><circle cx="100" cy="170" r="1.8" fill="${dk(c, .5)}"/>`;
      case 'jacket': return `<path d="${base}" fill="${c}" ${st}/><path d="M100 136V176" stroke="${dk(c, .5)}" stroke-width="2"/><path d="M76 150l-6 20M124 150l6 20" stroke="${K.cyan}" stroke-width="2.4" stroke-linecap="round"/>`;
      case 'apron': return `<path d="${base}" fill="${c}" ${st}/><path d="${rr(82, 144, 36, 32, 4)}" fill="${HAIR.ivory}"/><path d="M84 144L80 134M116 144L120 134" stroke="${HAIR.ivory}" stroke-width="2"/>`;
      case 'pilot': return `<path d="${base}" fill="${c}" ${st}/><path d="M70 132Q100 150 130 132L128 142Q100 156 72 142Z" fill="${HAIR.ivory}"/><path d="M100 148V176" stroke="${dk(c, .5)}" stroke-width="2"/>`;
      case 'sweater': return `<path d="${base}" fill="${c}" ${st}/><path d="M64 168H136" stroke="${dk(c, .25)}" stroke-width="5"/><path d="M84 132Q100 142 116 132" fill="none" stroke="${dk(c, .25)}" stroke-width="4"/>`;
      default: return `<path d="${base}" fill="${c}" ${st}/><path d="M94 138v10M106 138v10" stroke="${K.white}" stroke-opacity=".75" stroke-width="2" stroke-linecap="round"/><path d="M84 162h32" stroke="${dk(c, .3)}" stroke-width="2"/>`;
    }
  }

  function human(cfg, t, mini) {
    const ph = cfg.phase || 0, skin = SKIN[cfg.skin || 'light'], hair = HAIR[cfg.hair] || cfg.hair, hairD = dk(hair, .18), iris = IRIS[cfg.iris || 'coral'];
    const acc = cfg.acc || [], has = k => acc.includes(k);
    const br = Math.sin(2 * Math.PI * (t / 3.4 + ph));
    const bt = ((t + ph * 4.6) % 4.6) / 4.6, blink = bt < .03 ? 1 - bt / .03 : bt < .06 ? (bt - .03) / .03 : 1;
    const sway = 5 * Math.sin(2 * Math.PI * (t / 2.8 + ph)), hr = 1.6 * Math.sin(2 * Math.PI * (t / 4.4 + ph));
    const HT = `rotate(${n(hr)} 100 134)`, BT = `translate(100 176) scale(${n(1 - .01 * br)} ${n(1 + .018 * br)}) translate(-100 -176)`;
    const hst = `stroke="${dk(hair, .35)}" stroke-width="1.6" stroke-linejoin="round"`;
    const sk = `stroke="${dk(skin, .28)}" stroke-width="1.6"`;
    const back = [], body = [], head = [];
    const oc = cfg.outfitCol || '#3A3A46';
    if (has('hood')) back.push(`<path d="M38 102C34 42 70 22 100 22C130 22 166 42 162 102C164 126 150 140 140 146H60C50 140 36 126 38 102Z" fill="${oc}" stroke="${dk(oc)}" stroke-width="1.6"/>`);
    const style = has('hood') ? 'short' : cfg.style;
    if (style === 'twin' || style === 'side' || style === 'pony' || style === 'buns' || style === 'braid') back.push(`<path d="${BACK[style === 'braid' ? 'bob' : 'short']}" fill="${hairD}" ${hst}/>`);
    else back.push(`<path d="${BACK[style] || BACK.short}" fill="${hairD}" ${hst}/>`);
    if (style === 'twin') back.push(`<g transform="translate(50 70) rotate(${n(sway)})"><path d="${TAIL}" fill="${hair}" ${hst}/></g><g transform="translate(150 70) scale(-1 1) rotate(${n(sway)})"><path d="${TAIL}" fill="${hair}" ${hst}/></g><circle cx="52" cy="70" r="5" fill="${K.coral}"/><circle cx="148" cy="70" r="5" fill="${K.coral}"/>`);
    if (style === 'side') back.push(`<g transform="translate(152 78) scale(-1 1) rotate(${n(sway)})"><path d="${TAIL}" fill="${hair}" ${hst}/></g><circle cx="150" cy="78" r="5" fill="${K.coral}"/>`);
    if (style === 'pony') back.push(`<g transform="translate(128 44) rotate(${n(sway * .8)})"><path d="M0 0C24 -6 40 14 36 44C34 64 24 80 18 86C22 60 18 30 -4 14Z" fill="${hair}" ${hst}/></g><circle cx="128" cy="44" r="5" fill="${K.coral}"/>`);
    if (style === 'buns') back.push(`<circle cx="62" cy="42" r="17" fill="${hair}" ${hst}/><circle cx="138" cy="42" r="17" fill="${hair}" ${hst}/>`);
    // body
    body.push(`<ellipse cx="60" cy="154" rx="9" ry="17" transform="rotate(14 60 154)" fill="${cfg.outfit === 'sailor' || cfg.outfit === 'coat' ? K.white : oc}" stroke="${dk(oc)}" stroke-width="1.4"/><ellipse cx="140" cy="154" rx="9" ry="17" transform="rotate(-14 140 154)" fill="${cfg.outfit === 'sailor' || cfg.outfit === 'coat' ? K.white : oc}" stroke="${dk(oc)}" stroke-width="1.4"/>`);
    body.push(`<path d="${rr(92, 124, 16, 14, 4)}" fill="${skin}"/>`);
    body.push(outfit(cfg.outfit, oc));
    body.push(`<circle cx="57" cy="169" r="6" fill="${skin}" ${sk}/><circle cx="143" cy="169" r="6" fill="${skin}" ${sk}/>`);
    if (has('scarf')) body.push(`<path d="${rr(66, 126, 68, 13, 6.5)}" fill="${K.coral}"/><path d="M118 134l6 26" stroke="${K.coral}" stroke-width="10" stroke-linecap="round"/>`);
    // head
    if (has('elf')) head.push(`<path d="M52 92L30 78L50 108Z" fill="${skin}" ${sk} stroke-linejoin="round"/><path d="M148 92L170 78L150 108Z" fill="${skin}" ${sk} stroke-linejoin="round"/>`);
    else head.push(`<ellipse cx="50" cy="100" rx="6" ry="9" fill="${skin}" ${sk}/><ellipse cx="150" cy="100" rx="6" ry="9" fill="${skin}" ${sk}/>`);
    if (has('mech')) head.push(`<path d="${rr(38, 84, 14, 30, 5)}" fill="#46465A" stroke="#5E5E70" stroke-width="1.6"/><path d="${rr(148, 84, 14, 30, 5)}" fill="#46465A" stroke="#5E5E70" stroke-width="1.6"/><path d="M45 84V66M155 84V66" stroke="#5E5E70" stroke-width="2.5" stroke-linecap="round"/><circle cx="45" cy="64" r="3.5" fill="${K.cyan}"/><circle cx="155" cy="64" r="3.5" fill="${K.cyan}"/><path d="M42 100h6M152 100h6" stroke="${K.cyan}" stroke-width="2.5" stroke-linecap="round"/>`);
    head.push(`<ellipse cx="100" cy="90" rx="50" ry="46" fill="${skin}" ${sk}/>`);
    const ey = 100, eyeT = cfg.eyes || 'sparkle';
    if (has('visor')) head.push(`<path d="${rr(58, 88, 84, 24, 12)}" fill="${K.ink}"/><path d="${rr(60, 90, 80, 20, 10)}" fill="${K.cyan}" opacity=".35"/><path d="M68 100h18M114 100h18" stroke="${K.cyan}" stroke-width="${n(Math.max(1, 5 * blink))}" stroke-linecap="round"/>`);
    else { head.push(eye(80, ey, eyeT, iris, blink, skin, -1)); head.push(eye(120, ey, eyeT, iris, blink, skin, 1)); }
    head.push(blush(70, 130, 114));
    head.push(mouth(cfg.mouth || 'smile', 100, 121));
    if (style === 'bob' || style === 'long' || style === 'wavy' || style === 'twin' || style === 'braid') head.push(`<path d="${LOCK}" fill="${hair}" ${hst}/><path d="${LOCK}" transform="translate(200 0) scale(-1 1)" fill="${hair}" ${hst}/>`);
    if (style === 'braid') head.push(`<g transform="rotate(${n(sway * .4)} 142 118)">${[0, 1, 2, 3, 4].map(i => `<ellipse cx="${142 + i * 1.5}" cy="${122 + i * 11}" rx="${7 - i * .6}" ry="7" fill="${hair}" ${hst}/>`).join('')}<circle cx="148" cy="178" r="4" fill="${K.coral}"/></g>`);
    head.push(`<path d="${BANGS[cfg.bangs || 'jagged']}" fill="${hair}" ${hst}/>`);
    if (!mini) head.push(`<path d="M70 50C80 40 96 36 110 38" fill="none" stroke="#fff" stroke-opacity=".22" stroke-width="5" stroke-linecap="round"/>`);
    if (cfg.ahoge) head.push(`<path transform="rotate(${n(sway * 1.6)} 102 34)" d="M102 34C98 18 108 8 120 14" fill="none" stroke="${hair}" stroke-width="4.5" stroke-linecap="round"/>`);
    // accessories
    if (has('clip')) head.push(`<path transform="rotate(-24 128 56)" d="${rr(118, 52, 22, 7, 2.5)}" fill="${K.coral}"/>`);
    if (has('ribbon')) head.push(`<g transform="translate(132 40) rotate(${n(10 + sway * .5)})"><path d="M0 0L-16 -9L-14 9Z M0 0L16 -9L14 9Z" fill="${K.coral}" stroke="${dk(K.coral)}" stroke-width="1.4" stroke-linejoin="round"/><circle r="4" fill="${dk(K.coral, .15)}"/></g>`);
    if (has('star')) head.push(`<path transform="translate(66 52) scale(.9)" d="M0 -9L2.6 -3L9 -2.8L4 1.4L5.6 8L0 4.4L-5.6 8L-4 1.4L-9 -2.8L-2.6 -3Z" fill="${K.amber}" stroke-linejoin="round"/>`);
    if (has('moon')) head.push(`<path transform="translate(130 52)" d="M4 -9A9 9 0 1 0 4 9A7 7 0 1 1 4 -9Z" fill="${K.amber}"/>`);
    if (has('flower')) head.push(`<g transform="translate(66 54)">${[0, 72, 144, 216, 288].map(a => `<ellipse cy="-6" rx="4" ry="6" transform="rotate(${a})" fill="${K.coral}"/>`).join('')}<circle r="3" fill="${K.amber}"/></g>`);
    if (has('circlet')) head.push(`<path d="M54 64Q100 50 146 64" fill="none" stroke="${HAIR.silver}" stroke-width="2.5"/><path d="M100 50l5 6-5 6-5-6z" fill="${K.cyan}"/>`);
    if (has('goggles')) head.push(`<path d="M50 60Q100 46 150 60" fill="none" stroke="#46465A" stroke-width="6"/><circle cx="82" cy="54" r="10" fill="${K.ink}" stroke="#5E5E70" stroke-width="2.5"/><circle cx="118" cy="54" r="10" fill="${K.ink}" stroke="#5E5E70" stroke-width="2.5"/><circle cx="82" cy="54" r="6.5" fill="${K.cyan}" opacity=".7"/><circle cx="118" cy="54" r="6.5" fill="${K.cyan}" opacity=".7"/>`);
    if (has('glasses')) head.push(`<circle cx="80" cy="${ey}" r="13" fill="${K.cyan}" fill-opacity=".08" stroke="${K.ink}" stroke-width="2.2"/><circle cx="120" cy="${ey}" r="13" fill="${K.cyan}" fill-opacity=".08" stroke="${K.ink}" stroke-width="2.2"/><path d="M93 ${ey}h14" stroke="${K.ink}" stroke-width="2.2"/>`);
    if (has('foxears')) head.push(`<path d="M56 54L60 14L88 40Z" fill="${hair}" ${hst}/><path d="M62 46L64 26L78 40Z" fill="${HAIR.ivory}"/><path d="M144 54L140 14L112 40Z" fill="${hair}" ${hst}/><path d="M138 46L136 26L122 40Z" fill="${HAIR.ivory}"/>`);
    if (has('horns')) head.push(`<path d="M72 44C66 30 70 18 80 14C76 24 80 34 84 40Z" fill="${HAIR.ivory}" stroke="#BDB3A0" stroke-width="1.4"/><path d="M128 44C134 30 130 18 120 14C124 24 120 34 116 40Z" fill="${HAIR.ivory}" stroke="#BDB3A0" stroke-width="1.4"/>`);
    if (has('headphones') || has('catphones') || has('headset')) {
      head.push(`<path d="M46 92C44 26 156 26 154 92" fill="none" stroke="#46465A" stroke-width="6" stroke-linecap="round"/>`);
      if (has('catphones')) head.push(`<path d="M64 44L60 22L80 36Z M136 44L140 22L120 36Z" fill="#46465A" stroke="#46465A" stroke-width="3" stroke-linejoin="round"/><path d="M65 38L63 28L73 35Z M135 38L137 28L127 35Z" fill="${K.coral}"/>`);
      head.push(`<path d="${rr(36, 80, 18, 30, 8)}" fill="#34343F" stroke="#50505F" stroke-width="2"/>`);
      if (!has('headset')) head.push(`<path d="${rr(146, 80, 18, 30, 8)}" fill="#34343F" stroke="#50505F" stroke-width="2"/><circle cx="155" cy="95" r="4" fill="${has('catphones') ? K.coral : K.cyan}"/>`);
      else head.push(`<path d="${rr(146, 80, 18, 30, 8)}" fill="#34343F" stroke="#50505F" stroke-width="2"/><path d="M150 106C146 122 130 128 112 126" fill="none" stroke="#46465A" stroke-width="3" stroke-linecap="round"/><circle cx="110" cy="126" r="4" fill="${K.coral}"/>`);
      head.push(`<circle cx="45" cy="95" r="4" fill="${has('catphones') ? K.coral : K.cyan}"/>`);
    }
    if (has('beret')) head.push(`<ellipse cx="94" cy="40" rx="40" ry="14" transform="rotate(-10 94 40)" fill="${K.coral}" stroke="${dk(K.coral)}" stroke-width="1.6"/><path d="M94 26v-6" stroke="${dk(K.coral)}" stroke-width="3" stroke-linecap="round"/>`);
    if (has('witch')) head.push(`<ellipse cx="100" cy="46" rx="64" ry="12" fill="#2A2A33" stroke="#46465A" stroke-width="1.6"/><path d="M70 46L104 -6C108 -10 116 -6 120 0L114 4L130 46Z" fill="#2A2A33" stroke="#46465A" stroke-width="1.6" stroke-linejoin="round"/><path d="M71 40Q100 32 129 40L130 46Q100 38 70 46Z" fill="${K.coral}"/>`);
    if (has('halo')) head.push(`<ellipse cx="100" cy="${n(20 + 2 * Math.sin(2 * Math.PI * (t / 2 + ph)))}" rx="26" ry="6" fill="none" stroke="${K.cyan}" stroke-width="3.5"/>`);
    if (has('bubble')) head.push(`<circle cx="100" cy="88" r="64" fill="${K.cyan}" fill-opacity=".08" stroke="${K.cyan}" stroke-width="2.5"/><path d="M58 60C66 44 80 34 96 30" fill="none" stroke="#fff" stroke-opacity=".55" stroke-width="4" stroke-linecap="round"/>`);
    if (has('hood')) head.push(`<path d="M44 104C40 52 70 30 100 30C130 30 160 52 156 104" fill="none" stroke="${oc}" stroke-width="8" stroke-linecap="round"/>`);
    const sh = mini ? '' : `<ellipse cx="100" cy="178" rx="44" ry="5" fill="#000" opacity=".3"/>`;
    return { vb: mini ? '30 6 140 140' : '0 0 200 200', body: `${sh}<g transform="${HT}">${back.join('')}</g><g transform="${BT}">${body.join('')}</g><g transform="${HT}">${head.join('')}</g>` };
  }

  // ---------- anime creatures ----------
  const CB = {
    round: { d: 'M46 120a54 54 0 1 0 108 0a54 54 0 1 0-108 0Z', top: 66, eyeY: 114, ex: 20, ey2: 132 },
    egg: { d: 'M100 60C134 60 154 100 154 132C154 158 130 172 100 172C70 172 46 158 46 132C46 100 66 60 100 60Z', top: 60, eyeY: 116, ex: 19, ey2: 136 },
    bean: { d: 'M36 140C36 100 64 84 100 84C136 84 164 100 164 140C164 162 150 172 130 172H70C50 172 36 162 36 140Z', top: 84, eyeY: 126, ex: 22, ey2: 144 },
    drop: { d: 'M100 50C112 74 156 104 156 136C156 158 132 172 100 172C68 172 44 158 44 136C44 104 88 74 100 50Z', top: 58, eyeY: 124, ex: 19, ey2: 142 },
    square: { d: rr(46, 70, 108, 102, 24), top: 70, eyeY: 116, ex: 20, ey2: 140 },
    cloud: { d: 'M52 168C34 168 30 146 44 138C36 118 56 104 70 112C72 90 100 82 112 98C126 84 152 96 146 118C166 120 170 148 152 156C156 168 146 172 136 168Z', top: 86, eyeY: 130, ex: 20, ey2: 148 },
    star: { d: 'M100 52L118 98L166 100L128 130L142 174L100 148L58 174L72 130L34 100L82 98Z', top: 52, eyeY: 118, ex: 15, ey2: 134 },
    lantern: { d: rr(56, 72, 88, 98, 40), top: 72, eyeY: 116, ex: 18, ey2: 136 },
    flame: { d: 'M100 44C112 70 140 82 146 116C152 150 130 172 100 172C70 172 48 150 54 116C58 96 76 90 80 70C88 82 92 92 98 96C96 80 94 60 100 44Z', top: 52, eyeY: 134, ex: 16, ey2: 156 },
    wave: { d: 'M30 150C30 126 46 112 64 112C78 112 84 122 94 122C104 122 112 104 132 104C154 104 170 122 170 146C170 164 158 172 142 172H52C38 172 30 164 30 150Z', top: 104, eyeY: 140, ex: 18, ey2: 158 },
    hex: { d: 'M100 62L146 88V144L100 170L54 144V88Z', top: 62, eyeY: 118, ex: 17, ey2: 140 },
    chub: { d: 'M50 148C42 108 62 72 100 72C138 72 158 108 150 148C148 166 132 172 100 172C68 172 52 166 50 148Z', top: 72, eyeY: 108, ex: 17, ey2: 132 },
    loaf: { d: 'M34 150C34 126 58 116 90 116H128C152 116 168 128 168 148C168 164 156 172 140 172H50C40 172 34 164 34 150Z', top: 96, eyeY: 132, ex: 12, ey2: 150, fx: -30, earX: 19, earY: 108 },
    slime: { d: 'M28 160C28 110 62 80 100 80C138 80 172 110 172 160C172 170 162 175 150 173H50C38 175 28 170 28 160Z', top: 80, eyeY: 128, ex: 24, ey2: 150 },
    shade: { d: 'M50 132C46 92 66 66 100 66C134 66 154 92 150 132C154 154 142 172 120 172H80C58 172 46 154 50 132Z', top: 66, eyeY: 112, ex: 25, ey2: 146 },
    mote: { d: 'M62 132a38 38 0 1 0 76 0a38 38 0 1 0-76 0Z', top: 94, eyeY: 130, ex: 16, ey2: 150 }
  };
  function creature(cfg, t, mini) {
    const ph = cfg.phase || 0, b = CB[cfg.body], c = cfg.col || '#3A3A46', ex = cfg.extras || [], has = k => ex.includes(k);
    const br = Math.sin(2 * Math.PI * (t / 3.4 + ph)), bt = ((t + ph * 4.6) % 4.6) / 4.6, blink = bt < .03 ? 1 - bt / .03 : bt < .06 ? (bt - .03) / .03 : 1;
    const wig = 6 * Math.sin(2 * Math.PI * (t / 2.6 + ph)), fy = cfg.float ? -6 - 5 * Math.sin(2 * Math.PI * (t / 3 + ph)) : 0;
    const st = `stroke="${cfg.rim || dk(c, .3)}" stroke-width="${cfg.rim ? 2.5 : 2}" stroke-linejoin="round"`, g = [];
    const tail = mini ? '' : (cfg.tail === 'cat' ? `<g transform="translate(146 158) rotate(${n(-wig)})"><path d="M0 0C16 0 26 -10 26 -28C26 -38 31 -45 38 -44" fill="none" stroke="${dk(c, .3)}" stroke-width="12" stroke-linecap="round"/><path d="M0 0C16 0 26 -10 26 -28C26 -38 31 -45 38 -44" fill="none" stroke="${c}" stroke-width="8.5" stroke-linecap="round"/></g>`
      : cfg.tail === 'fox' ? `<g transform="translate(144 156) rotate(${n(-wig)})"><path d="M0 0C24 6 44 -8 44 -34C44 -46 36 -52 30 -50C30 -30 20 -14 -2 -12Z" fill="${c}" ${st}/><path d="M44 -34C44 -46 36 -52 30 -50C30 -44 32 -38 36 -34C40 -32 43 -32 44 -34Z" fill="#fff"/></g>`
      : cfg.tail === 'flame' ? `<g transform="translate(140 150) rotate(${n(-wig * 1.4)})"><path d="M0 0C20 -2 30 -20 26 -40C34 -30 40 -14 34 0C30 10 14 14 0 8Z" fill="${K.cyan}" opacity=".85"/><path d="M4 2C14 0 20 -10 20 -22C24 -14 26 -4 20 4C16 8 8 8 4 6Z" fill="#fff" opacity=".6"/></g>`
      : cfg.tail === 'curl' ? `<g transform="translate(160 130) rotate(${n(-wig)})"><circle r="17" fill="${c}" ${st}/><circle cx="3" cy="-2" r="8" fill="${cfg.cream || '#F6EEE2'}"/></g>` : cfg.tail === 'puff' ? `<g transform="translate(148 154) rotate(${n(-wig)})"><path d="M0 0C18 -4 30 -22 26 -40C34 -30 38 -10 28 4C20 12 6 10 0 6Z" fill="${c}" ${st}/><path d="M26 -40C34 -30 37 -18 34 -8C30 -16 28 -28 26 -40Z" fill="${cfg.cream || '#F6EEE2'}"/></g>` : cfg.tail === 'ring' ? `<g transform="translate(146 156) rotate(${n(-wig)})"><path d="M0 0C20 4 32 -12 28 -30" fill="none" stroke="${c}" stroke-width="14" stroke-linecap="round"/><path d="M14 0l4 -10M24 -10l8 -4M28 -22l8 0" stroke="${K.ink}" stroke-width="4" stroke-linecap="round"/></g>` : '');
    g.push(tail);
    if (has('wings') && !mini) g.push(`<path transform="rotate(${n(-wig)} 52 ${b.eyeY})" d="M52 ${b.eyeY}C34 ${b.eyeY - 30} 18 ${b.eyeY - 20} 20 ${b.eyeY - 6}C28 ${b.eyeY - 10} 32 ${b.eyeY} 30 ${b.eyeY + 8}C40 ${b.eyeY + 2} 46 ${b.eyeY + 10} 52 ${b.eyeY + 14}Z" fill="${dk(c, .15)}" ${st}/><path transform="translate(200 0) scale(-1 1) rotate(${n(-wig)} 52 ${b.eyeY})" d="M52 ${b.eyeY}C34 ${b.eyeY - 30} 18 ${b.eyeY - 20} 20 ${b.eyeY - 6}C28 ${b.eyeY - 10} 32 ${b.eyeY} 30 ${b.eyeY + 8}C40 ${b.eyeY + 2} 46 ${b.eyeY + 10} 52 ${b.eyeY + 14}Z" fill="${dk(c, .15)}" ${st}/>`);
    g.push(`<g transform="translate(${b.fx || 0} 0)">`);
    const ears = cfg.ears, eX = b.earX || 26, eY = b.earY || b.top + 14, inner = cfg.inner || K.coral;
    const pair = m => `<g transform="translate(${100 - eX} ${eY}) rotate(${n(-wig * .3)})">${m}</g><g transform="translate(${100 + eX} ${eY}) scale(-1 1) rotate(${n(-wig * .3)})">${m}</g>`;
    if (ears === 'cat') g.push(pair(`<path d="M-14 8C-17 -14 -12 -32 -6 -34C0 -35 8 -20 14 2Z" fill="${c}" ${st}/><path d="M-8 2C-10 -11 -8 -22 -5 -24C-1 -24 3 -14 7 0Z" fill="${inner}" opacity=".9"/>`));
    if (ears === 'fox') g.push(pair(`<path d="M-15 10C-17 -20 -8 -44 -2 -46C4 -46 10 -20 15 4Z" fill="${c}" ${st}/><path d="M-8 4C-9 -16 -5 -34 -2 -36C1 -36 5 -16 8 2Z" fill="${inner}" opacity=".9"/>`));
    if (ears === 'bunny') g.push(pair(`<g transform="rotate(${n(-6 + wig * .5)})"><path d="${rr(-9, -64, 18, 70, 9)}" fill="${c}" ${st}/><path d="${rr(-4.5, -56, 9, 52, 4.5)}" fill="${inner}" opacity=".85"/></g>`));
    if (ears === 'round') g.push(pair(`<circle cy="-6" r="15" fill="${c}" ${st}/><circle cy="-6" r="8" fill="${inner}" opacity=".85"/>`));
    if (ears === 'tuft') g.push(pair(`<path d="M-12 10L-10 -22L12 6Z" fill="${c}" ${st}/>`));
    if (ears === 'foxdark') g.push(pair(`<path d="M-15 10C-17 -20 -8 -44 -2 -46C4 -46 10 -20 15 4Z" fill="${c}" ${st}/><path d="M-12 -22C-10 -36 -5 -45 -2 -46C4 -46 8 -34 10 -22C4 -26 -4 -26 -12 -22Z" fill="${cfg.dark || '#4A2E26'}"/><path d="M-7 4C-8 -10 -5 -20 -2 -22C1 -22 4 -10 7 2Z" fill="${cfg.cream || '#F6EEE2'}"/>`));
    if (ears === 'smallround') g.push(pair(`<circle cy="-2" r="11" fill="${c}" ${st}/><circle cy="-2" r="5.5" fill="${inner}" opacity=".85"/>`));
    if (ears === 'horns') g.push(pair(`<path d="M-4 8C-10 -6 -6 -20 4 -24C2 -14 6 -4 8 4Z" fill="${HAIR.ivory}" stroke="#BDB3A0" stroke-width="1.6"/>`));
    if (ears === 'boltcat') g.push(pair(`<path d="M-14 8L-6 -30L14 2Z" fill="${c}" ${st}/><circle cx="-4" cy="-8" r="3" fill="${K.cyan}"/>`));
    if (cfg.top === 'antenna') g.push(`<path d="M100 ${b.top + 4}V${b.top - 24}" stroke="${dk(c, .3)}" stroke-width="3.5" stroke-linecap="round"/><circle cx="100" cy="${b.top - 28}" r="6" fill="${K.coral}"/>`);
    if (cfg.top === 'prop') g.push(`<path d="M100 ${b.top + 4}V${b.top - 12}" stroke="${dk(c, .3)}" stroke-width="3.5" stroke-linecap="round"/><ellipse cx="100" cy="${b.top - 14}" rx="${n(30 * Math.abs(Math.cos(t * 9)) + 4)}" ry="4" fill="${K.cyan}" opacity=".7"/>`);
    if (cfg.top === 'leaf') g.push(`<path d="M100 ${b.top + 4}V${b.top - 12}" stroke="${K.green}" stroke-width="3.5" stroke-linecap="round"/><path d="M100 ${b.top - 10}C92 ${b.top - 26} 78 ${b.top - 26} 74 ${b.top - 18}C80 ${b.top - 8} 92 ${b.top - 6} 100 ${b.top - 10}Z" fill="${K.green}"/><path d="M100 ${b.top - 12}C108 ${b.top - 30} 122 ${b.top - 30} 126 ${b.top - 22}C120 ${b.top - 12} 108 ${b.top - 8} 100 ${b.top - 12}Z" fill="${K.green}" opacity=".8"/>`);
    if (cfg.top === 'handle') g.push(`<path d="M80 ${b.top + 2}C80 ${b.top - 22} 120 ${b.top - 22} 120 ${b.top + 2}" fill="none" stroke="${dk(c, .35)}" stroke-width="4"/>`);
    if (has('antlers')) g.push(`<path d="M74 ${b.top + 8}L66 ${b.top - 22}M68 ${b.top - 12}L54 ${b.top - 18}M67 ${b.top - 20}L74 ${b.top - 32}M126 ${b.top + 8}L134 ${b.top - 22}M132 ${b.top - 12}L146 ${b.top - 18}M133 ${b.top - 20}L126 ${b.top - 32}" fill="none" stroke="#E8CC96" stroke-width="5" stroke-linecap="round"/>`);
    g.push('</g>');
    if (has('rain') && !mini) for (let i = 0; i < 4; i++) { const ph2 = ((t * .9 + i / 4 + ph) % 1); g.push(`<path d="M${64 + i * 24} ${n(172 + ph2 * 22)}v6" stroke="${K.cyan}" stroke-width="3.5" stroke-linecap="round" opacity="${n(1 - ph2)}"/>`); }
    if (has('glow')) g.push(`<circle cx="100" cy="${b.eyeY + 6}" r="${n(70 + 3 * br)}" fill="${c}" opacity=".12"/>`);
    g.push(`<path d="${b.d}" fill="${c}" ${st}/>`);
    if (has('cream') && cfg.body === 'loaf') g.push(`<path d="M58 172C70 156 120 156 150 172Z" fill="${cfg.cream || '#F6EEE2'}"/>`);
    g.push(`<g transform="translate(${b.fx || 0} 0)">`);
    if (has('head')) g.push(`<circle cx="100" cy="${b.eyeY - 2}" r="33" fill="${c}" ${st}/>`);
    if (has('cheekwhite')) g.push(`<ellipse cx="${100 - b.ex - 6}" cy="${b.eyeY + 17}" rx="12" ry="8" fill="${cfg.cream || '#F6EEE2'}"/><ellipse cx="${100 + b.ex + 6}" cy="${b.eyeY + 17}" rx="12" ry="8" fill="${cfg.cream || '#F6EEE2'}"/><ellipse cx="100" cy="${b.eyeY + 21}" rx="11" ry="7" fill="${cfg.cream || '#F6EEE2'}"/>`);
    if (has('innerflame')) { const fs = .58 + .05 * Math.sin(t * 9 + ph * 7); g.push(`<path transform="translate(100 172) scale(${n(fs)} ${n(fs + .04 * Math.sin(t * 13))}) translate(-100 -172)" d="${b.d}" fill="${cfg.inner || K.amber}"/>`); }
    if (has('foam')) g.push(`<path d="M52 126C58 116 70 114 76 120M118 112C124 104 138 102 146 108" fill="none" stroke="#fff" stroke-width="5" stroke-linecap="round"/>`);
    if (has('swirl')) g.push(`<path d="M150 ${b.eyeY - 6}C170 ${b.eyeY - 10} 178 ${b.eyeY + 10} 164 ${b.eyeY + 14}C156 ${b.eyeY + 16} 154 ${b.eyeY + 6} 160 ${b.eyeY + 4}M44 ${b.eyeY + 24}C28 ${b.eyeY + 26} 24 ${b.eyeY + 42} 36 ${b.eyeY + 44}" fill="none" stroke="${cfg.swirlCol || '#BFD9E4'}" stroke-width="4" stroke-linecap="round" transform="translate(${n(4 * Math.sin(t * 2 + ph * 5))} 0)"/>`);
    if (has('speckle')) g.push(`<circle cx="62" cy="128" r="3" fill="${dk(c, .25)}"/><circle cx="138" cy="122" r="2.5" fill="${dk(c, .25)}"/><circle cx="126" cy="160" r="3.5" fill="${dk(c, .25)}"/><circle cx="74" cy="158" r="2.5" fill="${dk(c, .25)}"/>`);
    if (has('cracks')) g.push(`<path d="M56 132l14 6 6 12M144 124l-12 10 4 14M96 98l6 10-4 8" fill="none" stroke="${K.coral}" stroke-width="3.5" stroke-linecap="round" stroke-linejoin="round" opacity="${n(.7 + .3 * Math.sin(t * 3 + ph * 6))}"/>`);
    if (has('core')) g.push(`<path d="M72 150C66 128 84 112 100 120C112 104 138 118 132 138C142 150 128 164 112 158C98 168 78 164 72 150Z" fill="${dk(c, .22)}" opacity=".55"/>`);
    if (has('gloss')) g.push(`<ellipse cx="72" cy="104" rx="20" ry="11" transform="rotate(-24 72 104)" fill="#fff" opacity=".75"/><circle cx="96" cy="94" r="4.5" fill="#fff" opacity=".7"/><path d="M146 150C150 140 150 130 146 120" fill="none" stroke="#fff" stroke-opacity=".45" stroke-width="4" stroke-linecap="round"/>`);
    if (has('sprout')) g.push(`<path d="${rr(94, b.top - 20 + 2 * Math.sin(t * 4 + ph * 9), 12, 16, 3)}" fill="${K.coral}" opacity="${Math.floor((t + ph) / .55) % 2 ? .35 : 1}"/>`);
    if (has('ribs')) g.push(`<path d="M78 76Q68 120 78 166M122 76Q132 120 122 166M56 120H144" fill="none" stroke="${dk(c, .2)}" stroke-width="2.5"/>`);
    if (has('belly')) g.push(`<ellipse cx="100" cy="${n((b.eyeY + 172) / 2 + 12)}" rx="34" ry="${n((172 - b.eyeY) * .38)}" fill="${cfg.belly || '#fff'}"/>`);
    if (has('mask')) g.push(`<path d="M${100 - b.ex - 16} ${b.eyeY - 6}Q${100 - b.ex} ${b.eyeY - 16} 100 ${b.eyeY - 2}Q${100 + b.ex} ${b.eyeY - 16} ${100 + b.ex + 16} ${b.eyeY - 6}Q${100 + b.ex + 14} ${b.eyeY + 16} 100 ${b.eyeY + 8}Q${100 - b.ex - 14} ${b.eyeY + 16} ${100 - b.ex - 16} ${b.eyeY - 6}Z" fill="${dk(c, .45)}"/>`);
    if (has('crescent')) g.push(`<path transform="translate(100 ${b.top + 22})" d="M3 -7A7 7 0 1 0 3 7A5.5 5.5 0 1 1 3 -7Z" fill="${K.amber}"/>`);
    if (!mini && !has('screen')) g.push(`<path d="M${70} ${b.eyeY - 10}C72 ${b.top + 18} 84 ${b.top + 8} 96 ${b.top + 6}" fill="none" stroke="#fff" stroke-opacity="${has('shine') ? .6 : .12}" stroke-width="7" stroke-linecap="round"/>`);
    const sc = has('screen');
    if (sc) g.push(`<path d="${rr(100 - b.ex - 20, b.eyeY - 22, 2 * b.ex + 40, 46, 12)}" fill="${K.ink}" stroke="${dk(c, .3)}" stroke-width="2"/>`);
    const skin = c;
    const lookX = 4 * Math.sin(2 * Math.PI * (t / 6 + ph)), lookY = cfg.lookUp ? -5 : 1.5 * Math.sin(2 * Math.PI * (t / 5 + ph));
    const lineInk = cfg.lineInk || '#1E2A3A';
    if (cfg.eyes === 'big' || cfg.eyes === 'bigcalm') [100 - b.ex, 100 + b.ex].forEach(x => {
      const ry = (cfg.eyeR || 19) * 1.15 * Math.max(.06, blink), rx = cfg.eyeR || 19;
      g.push(`<ellipse cx="${x}" cy="${b.eyeY}" rx="${rx}" ry="${n(ry)}" fill="#EEF0E6" stroke="${K.ink}" stroke-width="2"/>`);
      if (blink > .25) g.push(`<ellipse cx="${n(x + lookX * rx / 10)}" cy="${n(b.eyeY + lookY + 2)}" rx="${n(rx * .55)}" ry="${n(Math.min(ry, rx * 1.15) * .58)}" fill="#0E0E12"/><circle cx="${n(x + lookX * rx / 10 - rx * .22)}" cy="${n(b.eyeY + lookY - rx * .25)}" r="${n(rx * .13)}" fill="#fff" opacity=".85"/>`);
      if (cfg.eyes === 'bigcalm') g.push(`<path d="M${x - rx - 2} ${b.eyeY - 2}Q${x} ${b.eyeY - rx * .9} ${x + rx + 2} ${b.eyeY - 2}L${x + rx + 2} ${b.eyeY - rx * 1.4}H${x - rx - 2}Z" fill="${c}"/><path d="M${x - rx} ${b.eyeY - 2}Q${x} ${b.eyeY - rx * .85} ${x + rx} ${b.eyeY - 2}" fill="none" stroke="${cfg.rim || K.ink}" stroke-width="2.5"/>`);
    });
    else if (cfg.eyes && cfg.eyes.startsWith('line-')) [100 - b.ex, 100 + b.ex].forEach((x, i) => {
      const m = i ? -1 : 1, L = cfg.eyes;
      const d = L === 'line-happy' ? 'M-11 3Q0 -7 11 3' : L === 'line-sleepy' ? 'M-11 -1Q0 5 11 -1' : L === 'line-sad' ? 'M-11 4Q0 -5 11 1' : L === 'line-focus' ? 'M-11 -3L11 2' : 'M-11 0H11';
      g.push(`<g transform="translate(${x} ${b.eyeY}) scale(${m} 1)"><path d="${d}" fill="none" stroke="${lineInk}" stroke-width="2.6" stroke-linecap="round"/>${L === 'line-sleepy' || L === 'line-flat' ? '<path d="M-9 2l-3 3M-4 3.5l-1.5 3.5M1 3.5l0 3.8" stroke="' + lineInk + '" stroke-width="1.6" stroke-linecap="round"/>' : ''}${L === 'line-focus' ? '<path d="M-10 -9L8 -5" stroke="' + lineInk + '" stroke-width="2.4" stroke-linecap="round"/>' : ''}</g>`);
    });
    else if (cfg.eyes === 'kdot') [100 - b.ex, 100 + b.ex].forEach(x => g.push(blink < .25 ? `<path d="M${x - 6} ${b.eyeY}Q${x} ${b.eyeY + 4} ${x + 6} ${b.eyeY}" fill="none" stroke="#3A2A26" stroke-width="3" stroke-linecap="round"/>` : `<ellipse cx="${x}" cy="${b.eyeY}" rx="8" ry="${n(9 * blink)}" fill="#3A2A26"/><circle cx="${x - 2.5}" cy="${n(b.eyeY - 3.5 * blink)}" r="2.8" fill="#fff"/>`));
    else if (cfg.eyes === 'kclosed') [100 - b.ex, 100 + b.ex].forEach(x => g.push(`<path d="M${x - 7} ${b.eyeY - 1}Q${x} ${b.eyeY + 5} ${x + 7} ${b.eyeY - 1}" fill="none" stroke="#3A2A26" stroke-width="3" stroke-linecap="round"/>`));
    else if (cfg.eyes === 'screen-cursor') [100 - b.ex, 100 + b.ex].forEach(x => { const h = Math.max(2.5, 20 * blink); g.push(`<path d="${rr(x - 6, b.eyeY - h / 2, 12, h, 3)}" fill="${K.cyan}"/>`); });
    else if (cfg.eyes === 'screen-anime') [100 - b.ex, 100 + b.ex].forEach((x, i) => g.push(`<g transform="translate(${x} ${b.eyeY})">${blink < .25 ? `<path d="M-9 0h18" stroke="${K.cyan}" stroke-width="3" stroke-linecap="round"/>` : `<ellipse rx="8" ry="${n(10 * blink)}" fill="${K.cyan}"/><circle cx="-3" cy="${n(-4 * blink)}" r="2.6" fill="${K.ink}" opacity=".7"/><circle cx="3" cy="${n(4 * blink)}" r="1.4" fill="${K.ink}" opacity=".6"/>`}</g>`));
    else { g.push(eye(100 - b.ex, b.eyeY, cfg.eyes || 'sparkle', IRIS[cfg.iris || 'coral'], blink, skin, -1, .82)); g.push(eye(100 + b.ex, b.eyeY, cfg.eyes || 'sparkle', IRIS[cfg.iris || 'coral'], blink, skin, 1, .82)); }
    if (has('collar')) g.push(`<path d="M${100 - b.ex - 22} ${b.ey2}Q100 ${b.ey2 + 13} ${100 + b.ex + 22} ${b.ey2}" fill="none" stroke="${K.coral}" stroke-width="7" stroke-linecap="round"/>`);
    if (!sc && !cfg.noBlush) g.push(blush(100 - b.ex - 14, 100 + b.ex + 14, b.eyeY + 15));
    if (has('beak')) g.push(`<path d="M93 ${b.eyeY + 12}H107L100 ${b.eyeY + 21}Z" fill="${K.amber}" stroke="${dk(K.amber, .2)}" stroke-width="1.6" stroke-linejoin="round"/>`);
    else if (!sc && cfg.mouth !== 'none') g.push(mouth(cfg.mouth || 'cat', 100, b.eyeY + (cfg.mouthDy || 16), .9));
    if (has('kitnose')) g.push(`<ellipse cx="100" cy="${b.eyeY + 11}" rx="4.5" ry="3.2" fill="#3A2A26"/>`);
    g.push('</g>');
    if (has('arms')) g.push(`<ellipse cx="80" cy="138" rx="9" ry="11" fill="${c}" ${st}/><ellipse cx="120" cy="138" rx="9" ry="11" fill="${c}" ${st}/>`);
    if (has('pads')) [76, 124].forEach(x => g.push(`<ellipse cx="${x}" cy="162" rx="15" ry="13" fill="${cfg.footCol || c}" ${st}/><ellipse cx="${x}" cy="165" rx="7" ry="5.5" fill="#E8A08A"/><circle cx="${x - 6}" cy="156" r="2.6" fill="#E8A08A"/><circle cx="${x}" cy="154" r="2.6" fill="#E8A08A"/><circle cx="${x + 6}" cy="156" r="2.6" fill="#E8A08A"/>`));
    if (has('battery') || has('batterylow')) { const low = has('batterylow'), bx = cfg.body === 'loaf' ? 62 : 76; g.push(`<path d="${rr(bx, 30, 46, 24, 6)}" fill="none" stroke="${K.white}" stroke-width="3.5"/><path d="${rr(bx + 46, 37, 5, 10, 2)}" fill="${K.white}"/>` + (low ? `<path d="${rr(bx + 6, 36, 9, 12, 2)}" fill="${K.coral}" opacity="${Math.floor(t / .6) % 2 ? .35 : 1}"/>` : [0, 1, 2].map(i => `<path d="${rr(bx + 6 + i * 12, 36, 9, 12, 2)}" fill="${K.green}"/>`).join(''))); }
    if (has('leafprop')) g.push(`<path transform="rotate(${n(wig * 2)} 156 150)" d="M150 160C146 146 156 136 168 136C168 150 160 158 150 160Z" fill="${K.coral}"/><path d="M150 160L162 144" stroke="#8A4A36" stroke-width="2" stroke-linecap="round"/>`);
    if (has('paws') && !mini) g.push(`<ellipse cx="82" cy="170" rx="11" ry="6" fill="${dk(c, .08)}" ${st}/><ellipse cx="118" cy="170" rx="11" ry="6" fill="${dk(c, .08)}" ${st}/>`);
    if (has('feet') && !mini) g.push(`<ellipse cx="86" cy="173" rx="9" ry="4.5" fill="${K.amber}"/><ellipse cx="114" cy="173" rx="9" ry="4.5" fill="${K.amber}"/>`);
    if (has('legs') && !mini) g.push(`<path d="${rr(74, 166, 13, 12, 3)}" fill="${dk(c, .3)}"/><path d="${rr(113, 166, 13, 12, 3)}" fill="${dk(c, .3)}"/>`);
    if (has('bolts')) g.push(`<circle cx="46" cy="${b.eyeY}" r="6" fill="${dk(c, .1)}" ${st}/><circle cx="154" cy="${b.eyeY}" r="6" fill="${dk(c, .1)}" ${st}/>`);
    const tr = `translate(100 ${n(172 + fy)}) scale(${n(1 - .012 * br)} ${n(1 + .028 * br)}) translate(-100 -172)`;
    const sh = mini ? '' : `<ellipse cx="100" cy="177" rx="${n(46 * (1 + fy / 60))}" ry="5" fill="#000" opacity="${n(.3 * (1 + fy / 40))}"/>`;
    return { vb: mini ? '30 26 140 152' : '0 0 200 200', body: `${sh}<g transform="${tr}">${g.join('')}</g>` };
  }

  function render(cfg, t, opts) {
    opts = opts || {};
    const r = cfg.kind === 'h' ? human(cfg, t, !!opts.mini) : creature(cfg, t, !!opts.mini);
    const s = opts.size || 160;
    return `<svg xmlns="http://www.w3.org/2000/svg" width="${s}" height="${s}" viewBox="${r.vb}" style="display:block;overflow:visible">${r.body}</svg>`;
  }

  const F = { M: 'Anime · mascots', N: 'Anime · spirits', O: 'Anime · mecha', P: 'Inspired by your references', Q: 'Kawaii', R: 'Elements' };
  const H = (o) => Object.assign({ kind: 'h' }, o), C = (o) => Object.assign({ kind: 'c' }, o);
  const L = [
    ['M1', 'Neko', C({ body: 'round', col: '#3A3A46', ears: 'cat', eyes: 'sparkle', iris: 'coral', tail: 'cat', extras: ['paws'] })],
    ['M2', 'Kitsune kit', C({ body: 'egg', col: '#ECE6DA', ears: 'fox', eyes: 'sparkle', iris: 'amber', tail: 'fox', extras: ['paws'] })],
    ['M3', 'Usagi', C({ body: 'egg', col: '#C9C9D4', ears: 'bunny', eyes: 'sparkle', iris: 'coral', mouth: 'o', extras: ['paws'] })],
    ['M4', 'Hamu', C({ body: 'round', col: '#D9A887', ears: 'round', eyes: 'happy', mouth: 'cat', extras: ['belly', 'paws'] })],
    ['M5', 'Fukurou', C({ body: 'egg', col: '#5A6A84', ears: 'tuft', eyes: 'sparkle', iris: 'amber', extras: ['beak', 'belly', 'feet'], belly: '#C9C9D4' })],
    ['M6', 'Pengin', C({ body: 'egg', col: '#2E3A55', eyes: 'sparkle', iris: 'cyan', extras: ['beak', 'belly', 'feet'] })],
    ['M7', 'Tanuki', C({ body: 'round', col: '#6A6A78', ears: 'round', eyes: 'sparkle', iris: 'amber', tail: 'ring', extras: ['mask', 'belly', 'paws'], belly: '#ECE6DA' })],
    ['M8', 'Dragonling', C({ body: 'bean', col: '#82C8DC', ears: 'horns', eyes: 'sparkle', iris: 'coral', mouth: 'fang', extras: ['wings', 'paws'] })],
    ['N1', 'Fox spirit', C({ body: 'drop', col: '#ECE6DA', ears: 'fox', eyes: 'calm', iris: 'cyan', tail: 'flame', float: 1 })],
    ['N2', 'Yūrei', C({ body: 'drop', col: '#E4E4E8', eyes: 'calm', iris: 'cyan', mouth: 'o', extras: ['glow'], float: 1 })],
    ['N3', 'Slime', C({ body: 'drop', col: '#82C8DC', eyes: 'sparkle', iris: 'coral', mouth: 'open', extras: ['shine'] })],
    ['N4', 'Cloud', C({ body: 'cloud', col: '#ECE6DA', eyes: 'happy', mouth: 'smile', float: 1 })],
    ['N5', 'Star', C({ body: 'star', col: '#E6BE50', eyes: 'sparkle', iris: 'coral', mouth: 'smile', float: 1 })],
    ['N6', 'Lantern', C({ body: 'lantern', col: '#D77757', top: 'handle', eyes: 'calm', iris: 'amber', mouth: 'smile', extras: ['ribs', 'glow'], float: 1 })],
    ['N7', 'Moss spirit', C({ body: 'round', col: '#3A3A46', top: 'leaf', eyes: 'sparkle', iris: 'green', mouth: 'smile' })],
    ['N8', 'Moon rabbit', C({ body: 'egg', col: '#ECE6DA', ears: 'bunny', eyes: 'starry', iris: 'amber', mouth: 'cat', extras: ['crescent'] })],
    ['O2', 'Screen bot', C({ body: 'square', col: '#3A3A46', top: 'antenna', eyes: 'screen-anime', extras: ['screen', 'legs'] })],
    ['O4', 'Mecha neko', C({ body: 'round', col: '#5A6A84', ears: 'boltcat', eyes: 'screen-cursor', extras: ['screen', 'bolts', 'legs'] })],
    ['O5', 'Drone pal', C({ body: 'bean', col: '#3A3A46', top: 'prop', eyes: 'sparkle', iris: 'cyan', mouth: 'smile', float: 1 })],
    ['O7', 'Terminal head', C({ body: 'square', col: '#E4E4E8', eyes: 'screen-cursor', extras: ['screen', 'legs', 'bolts'] })],
    ['P1', 'Shade', C({ body: 'shade', col: '#141418', rim: '#3A3A46', ears: 'cat', inner: '#82C8DC', eyes: 'big', mouth: 'none', noBlush: 1, extras: ['collar', 'paws'], tail: 'cat' })],
    ['P2', 'Shade, curious', C({ body: 'shade', col: '#141418', rim: '#3A3A46', ears: 'fox', inner: '#82C8DC', eyes: 'big', eyeR: 21, lookUp: 1, mouth: 'none', noBlush: 1, extras: ['paws'] })],
    ['P3', 'Ink mote', C({ body: 'mote', col: '#141418', rim: '#3A3A46', ears: 'cat', inner: '#D77757', eyes: 'big', eyeR: 12, mouth: 'none', noBlush: 1, extras: ['glow'], float: 1 })],
    ['P4', 'Shade, drowsy', C({ body: 'shade', col: '#141418', rim: '#3A3A46', ears: 'cat', inner: '#82C8DC', eyes: 'bigcalm', mouth: 'none', noBlush: 1, extras: ['collar', 'paws'] })],
    ['P5', 'Glim, content', C({ body: 'slime', col: '#82C8DC', rim: '#5FA8BE', eyes: 'line-happy', mouth: 'none', extras: ['core', 'gloss', 'sprout'] })],
    ['P6', 'Glim, sleepy', C({ body: 'slime', col: '#82C8DC', rim: '#5FA8BE', eyes: 'line-sleepy', mouth: 'none', extras: ['core', 'gloss', 'sprout'] })],
    ['P7', 'Glim, focused', C({ body: 'slime', col: '#82C8DC', rim: '#5FA8BE', eyes: 'line-focus', mouth: 'flat', mouthDy: 18, noBlush: 1, extras: ['core', 'gloss', 'sprout'] })],
    ['P8', 'Glim, sad', C({ body: 'slime', col: '#82C8DC', rim: '#5FA8BE', eyes: 'line-sad', mouth: 'o', mouthDy: 18, extras: ['core', 'gloss', 'sprout'] })],
    ['Q1', 'Napping shiba', C({ body: 'loaf', col: '#E3A05C', eyes: 'kclosed', mouth: 'none', ears: 'cat', inner: '#F6EEE2', tail: 'curl', extras: ['head', 'cheekwhite', 'kitnose', 'battery', 'cream'] })],
    ['Q2', 'Shiba', C({ body: 'chub', col: '#E3A05C', eyes: 'kdot', ears: 'cat', inner: '#F6EEE2', tail: 'curl', extras: ['cheekwhite', 'kitnose', 'belly', 'pads', 'arms'], belly: '#F6EEE2' })],
    ['Q3', 'Autumn fox', C({ body: 'round', col: '#E3965C', eyes: 'kdot', ears: 'foxdark', tail: 'puff', extras: ['cheekwhite', 'leafprop', 'paws'] })],
    ['Q4', 'Chubby fox', C({ body: 'chub', col: '#E3965C', eyes: 'kdot', ears: 'fox', inner: '#F6EEE2', tail: 'puff', extras: ['belly', 'pads', 'arms'], belly: '#F6EEE2' })],
    ['Q5', 'Polar bear', C({ body: 'chub', col: '#F4F2EE', rim: '#C9C3BA', eyes: 'kdot', ears: 'smallround', inner: '#E8A08A', extras: ['belly', 'pads', 'arms', 'kitnose'], belly: '#EFE3D6' })],
    ['Q6', 'Fawn', C({ body: 'chub', col: '#B07A52', eyes: 'kdot', ears: 'smallround', inner: '#E8CC96', extras: ['antlers', 'belly', 'pads', 'arms'], belly: '#EAD2AE', footCol: '#B07A52' })],
    ['Q7', 'Graphite cat', C({ body: 'chub', col: '#3A3A46', eyes: 'kdot', ears: 'cat', tail: 'cat', extras: ['collar', 'pads', 'arms'] })],
    ['Q8', 'Bunny', C({ body: 'chub', col: '#ECE6DA', rim: '#C9C3BA', eyes: 'kdot', ears: 'bunny', inner: '#E8A08A', extras: ['belly', 'pads', 'arms'], belly: '#FFFFFF' })],
    ['Q9', 'Owl', C({ body: 'chub', col: '#5A6A84', eyes: 'kdot', ears: 'tuft', extras: ['beak', 'belly', 'feet'], belly: '#C9C9D4' })],
    ['Q10', 'Bear cub', C({ body: 'chub', col: '#8A5A3E', eyes: 'kdot', ears: 'smallround', inner: '#E8CC96', extras: ['belly', 'pads', 'arms', 'kitnose'], belly: '#E8CC96' })],
    ['Q11', 'Napping cat', C({ body: 'loaf', col: '#3A3A46', eyes: 'kclosed', mouth: 'none', ears: 'cat', tail: 'cat', extras: ['head', 'batterylow'] })],
    ['Q12', 'Napping fox', C({ body: 'loaf', col: '#E3965C', eyes: 'kclosed', mouth: 'none', ears: 'foxdark', tail: 'curl', extras: ['head', 'cheekwhite', 'kitnose', 'battery'] })],
    ['R1', 'Ember', C({ body: 'flame', col: '#D77757', rim: '#B85F42', eyes: 'kdot', mouth: 'smile', extras: ['innerflame'], inner: '#E6BE50', noBlush: 1 })],
    ['R2', 'Pilot light', C({ body: 'flame', col: '#82C8DC', rim: '#5FA8BE', eyes: 'calm', iris: 'cyan', mouth: 'smile', extras: ['innerflame', 'glow'], inner: '#E8F6FA', float: 1 })],
    ['R3', 'Droplet', C({ body: 'drop', col: '#82C8DC', rim: '#5FA8BE', eyes: 'kdot', mouth: 'cat', extras: ['gloss'] })],
    ['R4', 'Stream', C({ body: 'wave', col: '#5FA8BE', rim: '#4A8CA2', eyes: 'line-sleepy', mouth: 'none', extras: ['foam'] })],
    ['R5', 'Breeze', C({ body: 'round', col: '#E4EEF2', rim: '#BFD9E4', eyes: 'happy', mouth: 'smile', extras: ['swirl'], float: 1 })],
    ['R6', 'Frost', C({ body: 'hex', col: '#CFE8F0', rim: '#9CCAD8', eyes: 'sparkle', iris: 'cyan', mouth: 'o', extras: ['gloss'] })],
    ['R7', 'Pebble', C({ body: 'bean', col: '#8A8A96', rim: '#6A6A76', eyes: 'kclosed', mouth: 'smile', extras: ['speckle'] })],
    ['R8', 'Bubble', C({ body: 'round', col: '#BFE3EE', rim: '#82C8DC', eyes: 'kdot', mouth: 'o', extras: ['gloss'], float: 1 })],
    ['R9', 'Raincloud', C({ body: 'cloud', col: '#C9C9D4', rim: '#9A9AA8', eyes: 'line-sad', mouth: 'none', extras: ['rain'], float: 1 })],
    ['R10', 'Magma', C({ body: 'bean', col: '#3A3A46', eyes: 'kdot', mouth: 'smile', extras: ['cracks', 'glow'], noBlush: 1 })]
  ];
  const OPTIONS = L.map(([id, name, cfg], i) => ({ id, name, family: F[id[0]], cfg: Object.assign({ phase: (i * .173) % 1 }, cfg) }));
  window.ElisaAnime = { OPTIONS, FAMILIES: F, render };
})();
