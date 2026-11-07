// Generated from design/Elisa Brand Kit/05-character/shade/motion/*.json + the v2 update overlay (keyframes t, value, ease). Do not edit by hand;
// `motion_tables_match_the_kit` checks these against the kit's JSON when the kit is present.
use super::motion::{Ch::*, Ease::*, Eye, Track, k, ke};

pub const IDLE: (u32, &[Track]) = (4000, &[
    Track { ch: Sy, keys: &[k(0.0, 1.0, Io), k(0.5, 1.035, Io), k(1.0, 1.0, Io)] },
    Track { ch: Sx, keys: &[k(0.0, 1.0, Io), k(0.5, 0.985, Io), k(1.0, 1.0, Io)] },
    Track { ch: EyeOpen, keys: &[k(0.0, 1.0, Io), k(0.78, 1.0, In), k(0.8, 0.05, Out), k(0.83, 1.0, Io), k(1.0, 1.0, Io)] },
    Track { ch: EarR, keys: &[k(0.0, 0.0, Io), k(0.45, 0.0, Out), k(0.5, -16.0, Back), k(0.6, 0.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: Tail, keys: &[k(0.0, 0.0, Io), k(0.5, 10.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: LookX, keys: &[k(0.0, 0.0, Io), k(0.25, 0.0, Io), k(0.32, -3.0, Io), k(0.6, -3.0, Io), k(0.68, 0.0, Io), k(1.0, 0.0, Io)] },
]);
pub const LISTENING: (u32, &[Track]) = (2000, &[
    Track { ch: Rot, keys: &[k(0.0, 6.0, Io), k(0.5, 9.0, Io), k(1.0, 6.0, Io)] },
    Track { ch: LookX, keys: &[k(0.0, -4.0, Io)] },
    Track { ch: LookY, keys: &[k(0.0, 5.0, Io)] },
    Track { ch: Y, keys: &[k(0.0, 0.0, Io), k(0.25, 2.5, Io), k(0.5, 0.0, Io), k(0.75, 2.5, Io), k(1.0, 0.0, Io)] },
    Track { ch: EarL, keys: &[k(0.0, -8.0, Io), k(0.5, -13.0, Io), k(1.0, -8.0, Io)] },
    Track { ch: EarR, keys: &[k(0.0, -8.0, Io), k(0.5, -13.0, Io), k(1.0, -8.0, Io)] },
]);
pub const THINKING: (u32, &[Track]) = (2400, &[
    Track { ch: LookX, keys: &[k(0.0, 4.0, Io)] },
    Track { ch: LookY, keys: &[k(0.0, -6.0, Io)] },
    Track { ch: Rot, keys: &[k(0.0, -4.0, Io), k(0.5, -6.0, Io), k(1.0, -4.0, Io)] },
    Track { ch: EyeOpen, keys: &[k(0.0, 0.8, Io)] },
    Track { ch: Sy, keys: &[k(0.0, 1.0, Io), k(0.5, 1.02, Io), k(1.0, 1.0, Io)] },
    Track { ch: Tail, keys: &[k(0.0, 0.0, Io), k(0.2, 14.0, Out), k(0.3, 0.0, Io), k(0.6, 0.0, Io), k(0.7, 14.0, Out), k(0.8, 0.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: P1, keys: &[k(0.0, 0.0, Io), k(0.1, 1.0, Io), k(0.7, 1.0, Io), k(0.85, 0.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: P2, keys: &[k(0.0, 0.0, Io), k(0.25, 0.0, Io), k(0.35, 1.0, Io), k(0.75, 1.0, Io), k(0.9, 0.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: P3, keys: &[k(0.0, 0.0, Io), k(0.45, 0.0, Io), k(0.55, 1.0, Io), k(0.8, 1.0, Io), k(0.95, 0.0, Io), k(1.0, 0.0, Io)] },
]);
pub const STREAMING: (u32, &[Track]) = (2400, &[
    Track { ch: Y, keys: &[k(0.0, 0.0, Io), k(0.25, -3.0, Io), k(0.5, 0.0, Io), k(0.75, -3.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: Sy, keys: &[k(0.0, 1.0, Io), k(0.25, 1.025, Io), k(0.5, 1.0, Io), k(0.75, 1.025, Io), k(1.0, 1.0, Io)] },
    Track { ch: LookX, keys: &[k(0.0, 5.0, Io)] },
    Track { ch: EyeOpen, keys: &[k(0.0, 0.85, Io)] },
    Track { ch: P1, keys: &[k(0.0, 0.0, Lin), k(0.9, 1.0, Io), k(1.0, 1.0, Io)] },
]);
pub const RUNNING: (u32, &[Track]) = (1600, &[
    Track { ch: LookX, keys: &[k(0.0, 6.0, Io)] },
    Track { ch: LookY, keys: &[k(0.0, 3.0, Io)] },
    Track { ch: EyeOpen, keys: &[k(0.0, 0.9, Io)] },
    Track { ch: Rot, keys: &[k(0.0, 2.0, Io), k(0.5, 4.0, Io), k(1.0, 2.0, Io)] },
    Track { ch: PawR, keys: &[k(0.0, 0.45, Io)] },
    Track { ch: EarR, keys: &[k(0.0, 0.0, Io), k(0.5, -6.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: P1, keys: &[k(0.0, 0.0, Lin), k(1.0, 1.0, Io)] },
]);
pub const PERMISSION: (u32, &[Track]) = (2000, &[
    Track { ch: Sy, keys: &[k(0.0, 1.05, Io)] },
    Track { ch: Y, keys: &[k(0.0, -2.0, Io)] },
    Track { ch: PawR, keys: &[k(0.0, 1.0, Io), k(0.5, 0.8, Io), k(1.0, 1.0, Io)] },
    Track { ch: EarL, keys: &[k(0.0, -10.0, Io)] },
    Track { ch: EarR, keys: &[k(0.0, -10.0, Io)] },
    Track { ch: Eye, keys: &[ke(0.0, Eye::Wide, Io)] },
    Track { ch: Rot, keys: &[k(0.0, -3.0, Io), k(0.5, 3.0, Io), k(1.0, -3.0, Io)] },
    Track { ch: P1, keys: &[k(0.0, 1.0, Io), k(0.5, 1.12, Io), k(1.0, 1.0, Io)] },
]);
pub const SUCCESS: (u32, &[Track]) = (1600, &[
    Track { ch: Y, keys: &[k(0.0, 0.0, Io), k(0.12, 3.0, Out), k(0.32, -24.0, In), k(0.52, 0.0, Out), k(0.62, 2.0, Io), k(0.72, 0.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: Sy, keys: &[k(0.0, 1.0, Io), k(0.12, 0.9, Out), k(0.3, 1.08, Io), k(0.52, 0.92, Out), k(0.62, 1.02, Io), k(0.72, 1.0, Io), k(1.0, 1.0, Io)] },
    Track { ch: Sx, keys: &[k(0.0, 1.0, Io), k(0.12, 1.08, Out), k(0.3, 0.95, Io), k(0.52, 1.06, Out), k(0.62, 0.99, Io), k(0.72, 1.0, Io), k(1.0, 1.0, Io)] },
    Track { ch: Eye, keys: &[ke(0.0, Eye::Happy, Io)] },
    Track { ch: EarL, keys: &[k(0.0, 0.0, Io), k(0.3, -12.0, Io), k(0.6, 0.0, Back), k(1.0, 0.0, Io)] },
    Track { ch: EarR, keys: &[k(0.0, 0.0, Io), k(0.3, -12.0, Io), k(0.6, 0.0, Back), k(1.0, 0.0, Io)] },
    Track { ch: P1, keys: &[k(0.0, 0.0, Io), k(0.3, 0.0, Back), k(0.45, 1.0, Io), k(0.85, 1.0, In), k(1.0, 0.0, Io)] },
    Track { ch: P2, keys: &[k(0.0, 0.0, Io), k(0.32, 0.0, Out), k(0.7, 1.0, Io), k(1.0, 1.0, Io)] },
]);
pub const ERROR: (u32, &[Track]) = (2000, &[
    Track { ch: Eye, keys: &[ke(0.0, Eye::Sad, Io)] },
    Track { ch: EarL, keys: &[k(0.0, 24.0, Io)] },
    Track { ch: EarR, keys: &[k(0.0, 24.0, Io)] },
    Track { ch: Sy, keys: &[k(0.0, 0.96, Io)] },
    Track { ch: LookY, keys: &[k(0.0, 4.0, Io)] },
    Track { ch: Tail, keys: &[k(0.0, -18.0, Io)] },
    Track { ch: X, keys: &[k(0.0, 0.0, Io), k(0.08, -4.0, Io), k(0.16, 4.0, Io), k(0.24, -3.0, Io), k(0.32, 2.0, Io), k(0.4, 0.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: P1, keys: &[k(0.0, 0.9, Io), k(0.1, 1.1, Out), k(0.2, 1.0, Io), k(1.0, 1.0, Io)] },
]);
pub const INTERRUPTED: (u32, &[Track]) = (1400, &[
    Track { ch: X, keys: &[k(0.0, 0.0, Out), k(0.1, -8.0, Io), k(0.6, -8.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: Y, keys: &[k(0.0, 0.0, Out), k(0.08, -9.0, In), k(0.18, 0.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: Eye, keys: &[ke(0.0, Eye::Wide, Io)] },
    Track { ch: EyeOpen, keys: &[k(0.0, 1.0, Io), k(0.08, 1.15, Io), k(0.6, 1.15, Io), k(1.0, 1.0, Io)] },
    Track { ch: EarL, keys: &[k(0.0, 0.0, Out), k(0.08, -20.0, Io), k(0.6, -20.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: EarR, keys: &[k(0.0, 0.0, Out), k(0.08, -20.0, Io), k(0.6, -20.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: Rot, keys: &[k(0.0, 0.0, Out), k(0.08, -6.0, Io), k(0.6, -6.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: P1, keys: &[k(0.0, 0.0, Back), k(0.1, 1.0, Io), k(0.7, 1.0, In), k(0.9, 0.0, Io), k(1.0, 0.0, Io)] },
]);
pub const MEMORY: (u32, &[Track]) = (2400, &[
    Track { ch: P1, keys: &[k(0.0, 0.0, Io), k(0.55, 1.0, In), k(0.7, 1.2, Io), k(1.0, 1.2, Io)] },
    Track { ch: P2, keys: &[k(0.0, 0.0, Io), k(0.08, 1.0, Io), k(0.65, 1.0, Io), k(0.72, 0.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: P3, keys: &[k(0.0, 0.0, Io), k(0.68, 0.0, Out), k(1.0, 1.0, Io)] },
    Track { ch: Glow, keys: &[k(0.0, 0.0, Io), k(0.68, 0.0, Out), k(0.76, 1.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: LookY, keys: &[k(0.0, 4.0, Io), k(0.5, -7.0, Io), k(0.7, -7.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: EarL, keys: &[k(0.0, 0.0, Io), k(0.68, 0.0, Back), k(0.78, -12.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: EarR, keys: &[k(0.0, 0.0, Io), k(0.68, 0.0, Back), k(0.78, -12.0, Io), k(1.0, 0.0, Io)] },
]);
pub const SLEEPING: (u32, &[Track]) = (4000, &[
    Track { ch: Eye, keys: &[ke(0.0, Eye::Closed, Io)] },
    Track { ch: Sy, keys: &[k(0.0, 0.86, Io), k(0.5, 0.9, Io), k(1.0, 0.86, Io)] },
    Track { ch: Sx, keys: &[k(0.0, 1.06, Io), k(0.5, 1.04, Io), k(1.0, 1.06, Io)] },
    Track { ch: EarL, keys: &[k(0.0, 20.0, Io)] },
    Track { ch: EarR, keys: &[k(0.0, 20.0, Io)] },
    Track { ch: Dim, keys: &[k(0.0, 0.45, Io)] },
    Track { ch: Tail, keys: &[k(0.0, 38.0, Io)] },
    Track { ch: P1, keys: &[k(0.0, 0.0, Lin), k(1.0, 1.0, Io)] },
]);
pub const WAKING: (u32, &[Track]) = (2400, &[
    Track { ch: Sy, keys: &[k(0.0, 0.86, Io), k(0.25, 0.86, In), k(0.45, 1.12, Out), k(0.6, 0.97, Io), k(0.7, 1.0, Io), k(1.0, 1.0, Io)] },
    Track { ch: Sx, keys: &[k(0.0, 1.06, Io), k(0.25, 1.06, In), k(0.45, 0.94, Out), k(0.6, 1.01, Io), k(0.7, 1.0, Io), k(1.0, 1.0, Io)] },
    Track { ch: Eye, keys: &[ke(0.0, Eye::Closed, Step), ke(0.45, Eye::Block, Io)] },
    Track { ch: EyeOpen, keys: &[k(0.0, 1.0, Io), k(0.45, 0.1, Out), k(0.52, 1.0, Io), k(0.6, 1.0, In), k(0.62, 0.1, Out), k(0.65, 1.0, Io), k(0.7, 1.0, In), k(0.72, 0.1, Out), k(0.75, 1.0, Io), k(1.0, 1.0, Io)] },
    Track { ch: EarL, keys: &[k(0.0, 20.0, Io), k(0.4, 20.0, Back), k(0.52, -12.0, Io), k(0.65, 0.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: EarR, keys: &[k(0.0, 20.0, Io), k(0.42, 20.0, Back), k(0.54, -12.0, Io), k(0.67, 0.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: Dim, keys: &[k(0.0, 0.45, Io), k(0.45, 0.45, Io), k(0.6, 0.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: Tail, keys: &[k(0.0, 38.0, Io), k(0.4, 38.0, Io), k(0.6, 0.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: P1, keys: &[k(0.0, 0.0, Io), k(0.45, 0.0, Out), k(0.8, 1.0, Io), k(1.0, 1.0, Io)] },
]);
pub const GREETING: (u32, &[Track]) = (2000, &[
    Track { ch: Y, keys: &[k(0.0, 0.0, Io), k(0.1, -10.0, Out), k(0.25, 0.0, In), k(1.0, 0.0, Io)] },
    Track { ch: PawR, keys: &[k(0.0, 0.0, Out), k(0.15, 1.0, Io), k(0.85, 1.0, In), k(1.0, 0.0, Io)] },
    Track { ch: P1, keys: &[k(0.0, 0.0, Io), k(0.2, 0.0, Io), k(0.3, 1.0, Io), k(0.4, -1.0, Io), k(0.5, 1.0, Io), k(0.6, -1.0, Io), k(0.7, 1.0, Io), k(0.8, 0.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: Eye, keys: &[ke(0.0, Eye::Block, Step), ke(0.2, Eye::Happy, Io), ke(0.85, Eye::Block, Io)] },
    Track { ch: EarL, keys: &[k(0.0, 0.0, Io), k(0.15, -12.0, Back), k(0.85, -12.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: EarR, keys: &[k(0.0, 0.0, Io), k(0.15, -12.0, Back), k(0.85, -12.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: Rot, keys: &[k(0.0, 0.0, Io), k(0.3, -4.0, Io), k(0.5, 4.0, Io), k(0.7, -4.0, Io), k(0.85, 0.0, Io), k(1.0, 0.0, Io)] },
]);
pub const READING: (u32, &[Track]) = (2400, &[
    Track { ch: LookY, keys: &[k(0.0, 6.0, Io)] },
    Track { ch: LookX, keys: &[k(0.0, -6.0, Lin), k(0.23, 6.0, Io), k(0.25, -6.0, Lin), k(0.48, 6.0, Io), k(0.5, -6.0, Lin), k(0.73, 6.0, Io), k(0.75, -6.0, Lin), k(0.98, 6.0, Io), k(1.0, -6.0, Io)] },
    Track { ch: EyeOpen, keys: &[k(0.0, 0.85, Io)] },
    Track { ch: Rot, keys: &[k(0.0, 2.0, Io)] },
    Track { ch: P1, keys: &[k(0.0, 0.0, Lin), k(1.0, 1.0, Io)] },
]);
pub const SEARCHING: (u32, &[Track]) = (2000, &[
    Track { ch: P1, keys: &[k(0.0, 0.0, Io), k(0.5, 1.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: LookX, keys: &[k(0.0, -7.0, Io), k(0.5, 7.0, Io), k(1.0, -7.0, Io)] },
    Track { ch: LookY, keys: &[k(0.0, 1.0, Io)] },
    Track { ch: Rot, keys: &[k(0.0, -3.0, Io), k(0.5, 3.0, Io), k(1.0, -3.0, Io)] },
    Track { ch: EarL, keys: &[k(0.0, -6.0, Io)] },
    Track { ch: EarR, keys: &[k(0.0, -6.0, Io)] },
]);
pub const EDITING: (u32, &[Track]) = (1600, &[
    Track { ch: LookY, keys: &[k(0.0, 6.0, Io)] },
    Track { ch: LookX, keys: &[k(0.0, 5.0, Io)] },
    Track { ch: EyeOpen, keys: &[k(0.0, 0.8, Io)] },
    Track { ch: P1, keys: &[k(0.0, 0.0, Lin), k(1.0, 1.0, Io)] },
    Track { ch: Y, keys: &[k(0.0, 0.0, Io), k(0.25, 1.5, Io), k(0.5, 0.0, Io), k(0.75, 1.5, Io), k(1.0, 0.0, Io)] },
    Track { ch: Rot, keys: &[k(0.0, 4.0, Io)] },
    Track { ch: PawR, keys: &[k(0.0, 0.55, Io)] },
]);
pub const BUILDING: (u32, &[Track]) = (1200, &[
    Track { ch: P1, keys: &[k(0.0, 0.0, In), k(0.36, 1.0, Io), k(0.5, 1.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: P2, keys: &[k(0.0, 1.0, Io), k(0.36, 0.0, Out), k(0.8, 1.0, Io), k(1.0, 1.0, Io)] },
    Track { ch: Sy, keys: &[k(0.0, 1.0, Io), k(0.36, 1.0, Out), k(0.42, 0.94, Out), k(0.6, 1.0, Io), k(1.0, 1.0, Io)] },
    Track { ch: Sx, keys: &[k(0.0, 1.0, Io), k(0.36, 1.0, Out), k(0.42, 1.04, Io), k(0.6, 1.0, Io), k(1.0, 1.0, Io)] },
    Track { ch: LookY, keys: &[k(0.0, 6.0, Io)] },
    Track { ch: LookX, keys: &[k(0.0, 6.0, Io)] },
    Track { ch: Eye, keys: &[ke(0.0, Eye::Block, Step), ke(0.36, Eye::Closed, Io), ke(0.46, Eye::Block, Io)] },
    Track { ch: PawR, keys: &[k(0.0, 0.7, Io)] },
]);
pub const TESTING: (u32, &[Track]) = (2000, &[
    Track { ch: LookX, keys: &[k(0.0, 7.0, Io)] },
    Track { ch: LookY, keys: &[k(0.0, 2.0, Io)] },
    Track { ch: P1, keys: &[k(0.0, 0.0, Lin), k(1.0, 1.0, Io)] },
    Track { ch: Rot, keys: &[k(0.0, 3.0, Io), k(0.5, 6.0, Io), k(1.0, 3.0, Io)] },
    Track { ch: EarR, keys: &[k(0.0, 0.0, Io), k(0.5, -8.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: EarL, keys: &[k(0.0, -4.0, Io)] },
]);
pub const CELEBRATING: (u32, &[Track]) = (1800, &[
    Track { ch: Y, keys: &[k(0.0, 0.0, Io), k(0.1, 4.0, Out), k(0.32, -34.0, In), k(0.55, 0.0, Out), k(0.64, 3.0, Io), k(0.74, 0.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: Sy, keys: &[k(0.0, 1.0, Io), k(0.1, 0.86, Out), k(0.3, 1.12, Io), k(0.55, 0.9, Out), k(0.66, 1.03, Io), k(0.76, 1.0, Io), k(1.0, 1.0, Io)] },
    Track { ch: Sx, keys: &[k(0.0, 1.0, Io), k(0.1, 1.1, Out), k(0.3, 0.94, Io), k(0.55, 1.08, Out), k(0.66, 0.99, Io), k(0.76, 1.0, Io), k(1.0, 1.0, Io)] },
    Track { ch: Eye, keys: &[ke(0.0, Eye::Happy, Io)] },
    Track { ch: EarL, keys: &[k(0.0, 0.0, Io), k(0.3, -16.0, Io), k(0.6, 0.0, Back), k(1.0, 0.0, Io)] },
    Track { ch: EarR, keys: &[k(0.0, 0.0, Io), k(0.3, -16.0, Io), k(0.6, 0.0, Back), k(1.0, 0.0, Io)] },
    Track { ch: PawR, keys: &[k(0.0, 0.0, Io), k(0.2, 1.0, Io), k(0.7, 1.0, Io), k(0.9, 0.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: P1, keys: &[k(0.0, 0.0, Io), k(0.22, 0.0, Out), k(1.0, 1.0, Io)] },
]);
pub const CONFUSED: (u32, &[Track]) = (2400, &[
    Track { ch: Rot, keys: &[k(0.0, 0.0, Io), k(0.15, -10.0, Back), k(0.45, -10.0, Io), k(0.6, 10.0, Back), k(0.9, 10.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: LookX, keys: &[k(0.0, 0.0, Io), k(0.15, -6.0, Io), k(0.45, -6.0, Io), k(0.6, 6.0, Io), k(0.9, 6.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: LookY, keys: &[k(0.0, -4.0, Io)] },
    Track { ch: EarL, keys: &[k(0.0, 0.0, Io), k(0.15, 10.0, Io), k(0.45, 10.0, Io), k(0.6, -8.0, Io), k(0.9, -8.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: EarR, keys: &[k(0.0, 0.0, Io), k(0.15, -8.0, Io), k(0.45, -8.0, Io), k(0.6, 10.0, Io), k(0.9, 10.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: Eye, keys: &[ke(0.0, Eye::Wide, Io)] },
    Track { ch: P1, keys: &[k(0.0, 0.0, Out), k(0.18, 1.0, Io), k(0.85, 1.0, In), k(1.0, 0.0, Io)] },
]);
pub const POKED: (u32, &[Track]) = (1400, &[
    Track { ch: X, keys: &[k(0.0, 0.0, Io), k(0.06, 6.0, Out), k(0.2, -3.0, Io), k(0.32, 1.5, Io), k(0.45, 0.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: Sx, keys: &[k(0.0, 1.0, Io), k(0.06, 1.14, Out), k(0.2, 0.94, Io), k(0.34, 1.03, Io), k(0.48, 1.0, Io), k(1.0, 1.0, Io)] },
    Track { ch: Sy, keys: &[k(0.0, 1.0, Io), k(0.06, 0.86, Out), k(0.2, 1.06, Io), k(0.34, 0.98, Io), k(0.48, 1.0, Io), k(1.0, 1.0, Io)] },
    Track { ch: Rot, keys: &[k(0.0, 0.0, Io), k(0.06, 8.0, Out), k(0.2, -5.0, Io), k(0.34, 2.0, Io), k(0.48, 0.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: Eye, keys: &[ke(0.0, Eye::Wide, Step), ke(0.5, Eye::Happy, Io), ke(0.94, Eye::Block, Io)] },
    Track { ch: LookX, keys: &[k(0.0, -6.0, Io), k(0.5, 0.0, Io)] },
    Track { ch: EarL, keys: &[k(0.0, 0.0, Io), k(0.06, 16.0, Out), k(0.45, 0.0, Back), k(1.0, 0.0, Io)] },
    Track { ch: EarR, keys: &[k(0.0, 0.0, Io), k(0.06, 16.0, Out), k(0.45, 0.0, Back), k(1.0, 0.0, Io)] },
    Track { ch: P1, keys: &[k(0.0, 0.0, Out), k(0.35, 1.0, Io), k(1.0, 1.0, Io)] },
]);
pub const DRINKING: (u32, &[Track]) = (2400, &[
    Track { ch: P1, keys: &[k(0.0, 0.0, Lin), k(1.0, 1.0, Io)] },
    Track { ch: P2, keys: &[k(0.0, 0.0, Io), k(0.2, 1.0, Io), k(0.72, 1.0, Io), k(0.88, 0.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: Rot, keys: &[k(0.0, -2.0, Io), k(0.2, -7.0, Io), k(0.72, -7.0, Io), k(0.88, -2.0, Io), k(1.0, -2.0, Io)] },
    Track { ch: LookY, keys: &[k(0.0, -6.0, Io)] },
    Track { ch: LookX, keys: &[k(0.0, 4.0, Io)] },
    Track { ch: Eye, keys: &[ke(0.0, Eye::Block, Step), ke(0.22, Eye::Closed, Io), ke(0.74, Eye::Block, Io)] },
    Track { ch: Y, keys: &[k(0.0, 0.0, Io), k(0.3, -1.5, Io), k(0.38, 0.0, Io), k(0.46, -1.5, Io), k(0.54, 0.0, Io), k(0.62, -1.5, Io), k(0.7, 0.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: EarL, keys: &[k(0.0, -4.0, Io)] },
    Track { ch: EarR, keys: &[k(0.0, -4.0, Io)] },
    Track { ch: PawR, keys: &[k(0.0, 0.6, Io)] },
]);
pub const EATING: (u32, &[Track]) = (1600, &[
    Track { ch: P1, keys: &[k(0.0, 0.0, Lin), k(1.0, 1.0, Io)] },
    Track { ch: Y, keys: &[k(0.0, 0.0, Io), k(0.25, 4.0, Io), k(0.5, 0.0, Io), k(0.75, 4.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: Sy, keys: &[k(0.0, 1.0, Io), k(0.25, 0.96, Io), k(0.5, 1.0, Io), k(0.75, 0.96, Io), k(1.0, 1.0, Io)] },
    Track { ch: Rot, keys: &[k(0.0, 4.0, Io)] },
    Track { ch: LookY, keys: &[k(0.0, 8.0, Io)] },
    Track { ch: LookX, keys: &[k(0.0, 4.0, Io)] },
    Track { ch: Eye, keys: &[ke(0.0, Eye::Block, Step), ke(0.2, Eye::Happy, Io), ke(0.48, Eye::Block, Io), ke(0.7, Eye::Happy, Io), ke(0.96, Eye::Block, Io)] },
    Track { ch: EarL, keys: &[k(0.0, 0.0, Io), k(0.25, 6.0, Io), k(0.5, 0.0, Io), k(0.75, 6.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: EarR, keys: &[k(0.0, 0.0, Io), k(0.25, 6.0, Io), k(0.5, 0.0, Io), k(0.75, 6.0, Io), k(1.0, 0.0, Io)] },
]);
pub const PLAYING: (u32, &[Track]) = (2000, &[
    Track { ch: P1, keys: &[k(0.0, 0.0, Lin), k(1.0, 1.0, Io)] },
    Track { ch: PawR, keys: &[k(0.0, 0.0, Io), k(0.12, 1.0, Out), k(0.28, 0.0, Io), k(0.62, 1.0, Out), k(0.78, 0.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: X, keys: &[k(0.0, 0.0, Io), k(0.12, 4.0, Io), k(0.3, 0.0, Io), k(0.62, 4.0, Io), k(0.8, 0.0, Io), k(1.0, 0.0, Io)] },
    Track { ch: LookX, keys: &[k(0.0, 4.0, Io), k(0.25, 8.0, Io), k(0.4, 5.0, Io), k(0.5, 4.0, Io), k(0.75, 8.0, Io), k(0.9, 5.0, Io), k(1.0, 4.0, Io)] },
    Track { ch: LookY, keys: &[k(0.0, 6.0, Io), k(0.28, -2.0, Io), k(0.45, 6.0, Io), k(0.78, -2.0, Io), k(0.95, 6.0, Io), k(1.0, 6.0, Io)] },
    Track { ch: EarL, keys: &[k(0.0, -10.0, Io)] },
    Track { ch: EarR, keys: &[k(0.0, -10.0, Io)] },
    Track { ch: Eye, keys: &[ke(0.0, Eye::Wide, Step), ke(0.4, Eye::Happy, Io), ke(0.52, Eye::Wide, Io), ke(0.9, Eye::Happy, Io), ke(1.0, Eye::Wide, Io)] },
    Track { ch: Rot, keys: &[k(0.0, 2.0, Io), k(0.12, 5.0, Io), k(0.3, 1.0, Io), k(0.62, 5.0, Io), k(0.8, 1.0, Io), k(1.0, 2.0, Io)] },
]);
