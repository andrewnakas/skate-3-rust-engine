//! The guest C runtime's float leaves that the audio thread reaches, and the hole where two of
//! them are.
//!
//! Three functions live at `0x82F4DE80`, `0x82F4DED0` and `0x82F4DFB0`. They are not audio code —
//! they are the image's `floor`, sine and cosine — but the spatial layer cannot be written without
//! them, so they get a module of their own rather than being buried in [`crate::spatial`].
//!
//! | | guest | does | `docs/ports.md` | calls/boot | calls/play |
//! |---|---|---|---|---|---|
//! | [`floor`] | `sub_82F4DE80` | `floor(double)`, register-only leaf | verified | 2,872,661 | 2,942,889 |
//! | [`Trig::sine`] | `sub_82F4DED0` | sine | **no `.inc` at all** | — | — |
//! | [`Trig::cosine`] | `sub_82F4DFB0` | cosine | **no `.inc` at all** | — | — |
//!
//! `sub_82F4DE80` is the hottest body anyone has ported in this project: 2.87 million calls in a
//! boot session on `RwAudioCore Dac`, more than the sine kernel and the two buffer multiplies put
//! together.
//!
//! ## The two that are not here, and why they are a trait
//!
//! `sub_82F4DED0` and `sub_82F4DFB0` are **outside the 216 audio-thread functions the native sweep
//! covered**, so neither language has a verified reference for them — the same position
//! `crate::dsp::biquad`'s delegate `sub_82B43978` is in. Writing a sine here would be new analysis
//! dressed as a transcription, and substituting `f64::sin` would be worse: it would turn every
//! future comparison of `sub_82B45788` and `sub_82B269C0` from a failure into a meaningless pass,
//! because the two functions differ in their last mantissa bits and the callers `frsp` the result
//! into a single where that difference survives about half the time.
//!
//! So they are a [`Trig`] parameter. [`Unported`] is the default and returns an `Err` naming the
//! guest address — the same choice `crate::eval::dispatch` makes for an unported opcode. [`Closures`]
//! lets a caller that *has* an implementation supply one. The trait takes `&Guest` because the real
//! bodies almost certainly read a polynomial out of the image, and a future port of them must be
//! able to.
//!
//! **Consequence for replay, stated rather than left to be discovered:** a recorded vector for
//! `sub_82B45788` or `sub_82B269C0` cannot be replayed against this crate until those two are
//! ported. Their results feed every value the two functions store.
//!
//! ## The constants, measured
//!
//! `probe/ports/notes/sub_82F4DE80.md` records that its two pool doubles were **not** read from the
//! image — "by shape they are 1.0 and 2^52" — while being careful to load both through the guest
//! map. Reading the validated dump (`probe/harness/out/image/`) at those addresses:
//!
//! | cell | address | inferred | measured |
//! |---|---|---|---|
//! | [`POOL_STEP_DOWN`] | `0x82010108` | 1.0 | **1.0** |
//! | [`POOL_INTEGRAL_MAGNITUDE`] | `0x820514D8` | 2^52 | **1e18** (`0x43ABC16D674EC800`) |
//!
//! The second inference is **wrong**, and the port is unaffected because it reads the cell live.
//! 1e18 is a coherent choice where 2^52 is only a plausible one: it is the largest round decimal
//! magnitude comfortably below `2^63`, which is the bound `fctidz` needs in order not to produce
//! its integer indefinite. Above it every double is already an integer anyway, so the arm that
//! returns `x` unchanged is correct for the whole range it covers. That is noted here and recorded
//! in this session's report; nothing else in the tree depends on the old guess.

use crate::vmx::Fpscr;
use crate::{Error, Guest, Result, fp};

// The two pool doubles. Each address is `((lis_imm & 0xFFFF) << 16) + offset`, computed from the
// immediate and asserted, never read off the disassembly.
const LIS_82010000: u32 = ((-32255i32 as u32) & 0xFFFF) << 16;
const LIS_82050000: u32 = ((-32251i32 as u32) & 0xFFFF) << 16;
const _: () = assert!(LIS_82010000 == 0x8201_0000, "lis -32255");
const _: () = assert!(LIS_82050000 == 0x8205_0000, "lis -32251");

/// `lfd f13,264(r11)` — measured 1.0, the step taken when the truncation rounded the wrong way.
pub const POOL_STEP_DOWN: u32 = LIS_82010000 + 264;
/// `lfd f0,5336(r10)` — measured 1e18, the magnitude above which a double is already integral.
pub const POOL_INTEGRAL_MAGNITUDE: u32 = LIS_82050000 + 5336;

const _: () = assert!(POOL_STEP_DOWN == 0x8201_0108);
const _: () = assert!(POOL_INTEGRAL_MAGNITUDE == 0x8205_14D8);

/// `sub_82F4DE80` — `floor(double)`.
///
/// `x` is `f1` and the result is `f1`; the mask is `kReturnF1`, so the returned value **is** the
/// whole of the comparison — this function has no stores at all. `r3`…`r8` carry nothing. It reads
/// the two pool doubles and nothing else.
///
/// Four steps, and the third is the one a reader would not write unprompted:
///
/// 1. `trunc = fcfid(fctidz(x))`, truncation toward zero through a 64-bit integer;
/// 2. `fsel` on the signed fraction steps down by [`POOL_STEP_DOWN`] when `x` was negative and had
///    one — which is what turns truncation into a floor;
/// 3. `fsel` on `1e18 - |x|` passes large magnitudes through **unchanged**, because they are
///    already integral and because `fctidz` above `2^63` would not be;
/// 4. `fsel` on `-|x|` passes `±0` through with its sign, since `-0.0 >= 0.0` holds and the
///    subtraction in step 2 would otherwise have turned `-0.0` into `+0.0`.
///
/// A NaN falls to the *else* side of every `fsel` — `>=` is false when unordered — so it comes out
/// through the step-3 arm as `x` itself, payload intact. The port keeps that as three selects
/// rather than collapsing it into `f64::floor`, which differs from this on `±0` and would hide the
/// whole point of steps 3 and 4.
pub fn floor(g: &Guest, x: f64) -> Result<f64> {
    let mut fpscr = Fpscr::capture();
    fpscr.disable_flush_mode_unconditional(); // emitted before fctidz

    let truncated_bits = fp::fctidz(x); // fctidz f12,f1
    let abs_x = fp::abs_double(x); // fabs f11,f1

    let step_down = fp::load_double(g, POOL_STEP_DOWN)?; // lfd f13,264(r11)
    let integral_mag = fp::load_double(g, POOL_INTEGRAL_MAGNITUDE)?; // lfd f0,5336(r10)

    let truncated = fp::fcfid(truncated_bits); // fcfid f12,f12
    // Plain `fsub`, not `fsubs`: nothing on this path narrows to single. All three subtractions
    // below are double, and the port must not "tidy" them into the `.s` forms the rest of the
    // crate is full of.
    let magnitude_margin = integral_mag - abs_x; // fsub f0,f0,f11
    let neg_abs_x = fp::neg_double(abs_x); // fneg f11,f11
    let fraction = x - truncated; // fsub f10,f1,f12
    let stepped_down = truncated - step_down; // fsub f13,f12,f13

    let floored = fp::fsel(fraction, truncated, stepped_down); // fsel f13,f10,f12,f13
    let bounded = fp::fsel(magnitude_margin, floored, x); // fsel f0,f0,f13,f1
    Ok(fp::fsel(neg_abs_x, x, bounded)) // fsel f1,f11,f1,f0
}

/// The image's sine and cosine, which **have no port in either language**.
///
/// `sub_82F4DED0` is the sine and `sub_82F4DFB0` the cosine; both take `f1` and return `f1`, and
/// `probe/ports/notes/sub_82B45788.md` records that each writes only its own red zone — no guest
/// state, no indirect call, no timebase. That is why the two callers here are gate-1 clean while
/// their trigonometry is not available: the census could see inside those bodies, but the sweep
/// never gave them an `.inc`, so there is no verified reference to transcribe.
///
/// Implementations take `&Guest` so that a real port of either can read the image the way every
/// other body in this crate does.
pub trait Trig {
    /// `sub_82F4DED0` — `f1` in, `f1` out.
    fn sine(&mut self, g: &Guest, x: f64) -> Result<f64>;
    /// `sub_82F4DFB0` — `f1` in, `f1` out.
    fn cosine(&mut self, g: &Guest, x: f64) -> Result<f64>;
}

/// The default: an `Err` naming the guest function, never a value.
///
/// Feeding `f64::sin` to a caller here would produce a number that is right to about fifteen
/// digits and wrong in the bits that survive the callers' `frsp`. A wrong answer that looks right
/// is exactly what this crate refuses to produce for an unported callee.
#[derive(Clone, Copy, Debug, Default)]
pub struct Unported;

impl Trig for Unported {
    fn sine(&mut self, _g: &Guest, _x: f64) -> Result<f64> {
        Err(Error::new(0x82F4_DED0, "sub_82F4DED0 (sine) has no verified port in either language"))
    }
    fn cosine(&mut self, _g: &Guest, _x: f64) -> Result<f64> {
        Err(Error::new(0x82F4_DFB0, "sub_82F4DFB0 (cosine) has no verified port in either language"))
    }
}

/// The image's own sine and cosine, transcribed from their C++ ports.
///
/// Those ports (`sub_82F4DED0`, `sub_82F4DFB0`) were written on 2026-09-13 and shadow-verified at
/// zero divergence: sine over 436,266 comparable calls under scripted play and 414,229 at boot,
/// cosine over 268,332 and 228,467. Both are a Cody-Waite reduction by pi in two parts followed by
/// an odd Taylor polynomial through x^19, over one table read live. Cosine shifts by pi/2 and
/// offsets the quotient by one half so the sine's polynomial serves unchanged.
///
/// This is what [`Unported`] was waiting for. Substituting `f64::sin` would agree to fifteen digits
/// and differ in the bits the callers keep; this reproduces the guest's own arithmetic instead,
/// down to the fused multiply-adds and the operand order of every multiply.
#[derive(Clone, Copy, Debug, Default)]
pub struct Image;

const TRIG_TABLE: u32 = 0x82FB_0000 + 20000; // lis r11,-32005 ; addi r11,r11,20000
const TRIG_ONE: u32 = 0x8201_0000 + 264; // lis r10,-32255 ; lfd 264(r10) = 1.0
const TRIG_NAN: u32 = 0x82FB_0000 + 22728; // lis r11,-32005 ; lfd 22728(r11) = NaN
const _: () = assert!(
    TRIG_TABLE == 0x82FB_4E20 && TRIG_ONE == 0x8201_0108 && TRIG_NAN == 0x82FB_58C8,
    "((imm & 0xFFFF) << 16) + offset, computed rather than read by eye"
);

/// `lfs`: a single widened exactly to double.
fn lfs(g: &Guest, ea: u32) -> Result<f64> {
    Ok(f32::from_bits(g.u32(ea)?) as f64)
}

/// The odd polynomial both functions share: `p(r^2) * r`, in the guest's Horner order.
fn trig_poly(g: &Guest, r: f64) -> Result<f64> {
    let one = fp::load_double(g, TRIG_ONE)?;
    let c9 = fp::load_double(g, TRIG_TABLE + 112)?;
    let c8 = fp::load_double(g, TRIG_TABLE + 104)?;
    let c7 = fp::load_double(g, TRIG_TABLE + 96)?;
    let c6 = fp::load_double(g, TRIG_TABLE + 88)?;
    let c5 = fp::load_double(g, TRIG_TABLE + 80)?;
    let c4 = fp::load_double(g, TRIG_TABLE + 72)?;
    let c3 = fp::load_double(g, TRIG_TABLE + 64)?;
    let c2 = fp::load_double(g, TRIG_TABLE + 56)?;
    let r2 = r * r; // fmul f13,f9,f9
    let mut p = c9.mul_add(r2, c8); // fmadd f11,f8,f13,f7
    p = p.mul_add(r2, c7);
    p = p.mul_add(r2, c6);
    p = p.mul_add(r2, c5);
    p = p.mul_add(r2, c4);
    p = p.mul_add(r2, c3);
    p = p.mul_add(r2, c2);
    p = p.mul_add(r2, one); // fmadd f3,f4,f13,f30 (f31 in the cosine)
    Ok(p * r) // fmul f13,f3,f9
}

/// `sub_82F4DED0`. `f1` in, `f1` out. `±0` returns the input unchanged, sign included.
fn image_sine(g: &Guest, x: f64) -> Result<f64> {
    let mut fpscr = Fpscr::capture();
    fpscr.disable_flush_mode_unconditional(); // at stfd f30,-16(r1)
    let abs_bits = x.to_bits() & !0x8000_0000_0000_0000;
    let ax = f64::from_bits(abs_bits); // fabs f0,f1
    let inv_pi = fp::load_double(g, TRIG_TABLE + 8)?; // lfd f11,8(r11)
    let minus_one = lfs(g, TRIG_TABLE + 32)?; // lfs f12,32(r11)
    let pi_hi = fp::load_double(g, TRIG_TABLE + 40)?; // lfd f10,40(r11)
    let q = inv_pi * ax; // fmul f13,f11,f0
    let pi_lo = fp::load_double(g, TRIG_TABLE + 48)?; // lfd f9,48(r11)
    let n_rounded = fp::fctid(q); // fctid f11,f13
    let plus_one = lfs(g, TRIG_TABLE + 28)?; // lfs f13,28(r11)
    let sign = fp::fsel(x, plus_one, minus_one); // fsel f12,f1,f13,f12
    let n = fp::fcfid(n_rounded); // fcfid f13,f11
    let r_hi = -pi_hi.mul_add(n, -ax); // fnmsub f11,f10,f13,f0
    let odd = (fp::fctidz(n) & 1) != 0; // fctidz ; stfd ; ld ; clrldi r8,r9,63
    let r = -pi_lo.mul_add(n, -r_hi); // fnmsub f9,f9,f13,f11
    let mut s = trig_poly(g, r)?;
    if odd { s = fp::neg_double(s); } // fneg f13,f13 (sine)
    fpscr.disable_flush_mode_unconditional(); // loc_82F4DF80
    let signed_s = s * sign; // fmul f12,f13,f12
    let mut result = x; // the +/-0 path never writes f1
    if abs_bits != 0 {
        let limit = fp::load_double(g, TRIG_TABLE + 16)?; // lfd f13,16(r11)
        let over = ax - limit; // fsub f13,f0,f13
        let nan = fp::load_double(g, TRIG_NAN)?; // lfd f0,22728(r11)
        result = fp::fsel(over, nan, signed_s); // fsel f1,f13,f0,f12
    }
    fpscr.disable_flush_mode_unconditional(); // loc_82F4DFA4
    Ok(result)
}

/// `sub_82F4DFB0`. `f1` in, `f1` out. `|x| == 0` returns exactly 1.0.
fn image_cosine(g: &Guest, x: f64) -> Result<f64> {
    let mut fpscr = Fpscr::capture();
    fpscr.disable_flush_mode_unconditional(); // at stfd f31,-8(r1)
    let ax = fp::abs_double(x); // fabs f0,f1
    let half_pi = fp::load_double(g, TRIG_TABLE)?; // lfd f13,0(r11)
    let half = lfs(g, TRIG_TABLE + 36)?; // lfs f11,36(r11)
    let pi_hi = fp::load_double(g, TRIG_TABLE + 40)?; // lfd f10,40(r11)
    let shifted = half_pi + ax; // fadd f12,f13,f0
    let inv_pi = fp::load_double(g, TRIG_TABLE + 8)?; // lfd f13,8(r11)
    let pi_lo = fp::load_double(g, TRIG_TABLE + 48)?; // lfd f9,48(r11)
    let q = inv_pi * shifted; // fmul f13,f13,f12
    let n = fp::fcfid(fp::fctid(q)); // fctid ; fcfid
    let m = n - half; // fsub f11,f13,f11
    let odd = (fp::fctidz(n) & 1) != 0; // fctidz f13,f13 ; clrldi
    let r_hi = -pi_hi.mul_add(m, -ax); // fnmsub f10,f10,f11,f0
    let r = -pi_lo.mul_add(m, -r_hi); // fnmsub f9,f9,f11,f10
    let mut c = trig_poly(g, r)?;
    if odd { c = fp::neg_double(c); } // fneg f13,f13 (cosine)
    fpscr.disable_flush_mode_unconditional(); // loc_82F4E05C
    let zero = lfs(g, TRIG_TABLE + 24)?; // lfs f11,24(r11)
    if ax == zero {
        return lfs(g, TRIG_TABLE + 28); // fcmpu ; bne not taken ; lfs f1,28(r11) = 1.0
    }
    fpscr.disable_flush_mode_unconditional(); // loc_82F4E074
    let limit = fp::load_double(g, TRIG_TABLE + 16)?; // lfd f0,16(r11)
    let over = shifted - limit; // fsub f12,f12,f0
    let nan = fp::load_double(g, TRIG_NAN)?; // lfd f0,22728(r11)
    Ok(fp::fsel(over, nan, c)) // fsel f1,f12,f0,f13
}

impl Trig for Image {
    fn sine(&mut self, g: &Guest, x: f64) -> Result<f64> {
        image_sine(g, x)
    }
    fn cosine(&mut self, g: &Guest, x: f64) -> Result<f64> {
        image_cosine(g, x)
    }
}

/// Two caller-supplied implementations, for a caller that has them.
///
/// The closure form is what [`crate::player`] already uses for its two unportable callees. It takes
/// the pair explicitly so that nothing in this crate can reach a sine by default.
pub struct Closures<S, C> {
    sine: S,
    cosine: C,
}

impl<S, C> Closures<S, C>
where
    S: FnMut(&Guest, f64) -> Result<f64>,
    C: FnMut(&Guest, f64) -> Result<f64>,
{
    pub fn new(sine: S, cosine: C) -> Self {
        Self { sine, cosine }
    }
}

impl<S, C> Trig for Closures<S, C>
where
    S: FnMut(&Guest, f64) -> Result<f64>,
    C: FnMut(&Guest, f64) -> Result<f64>,
{
    fn sine(&mut self, g: &Guest, x: f64) -> Result<f64> {
        (self.sine)(g, x)
    }
    fn cosine(&mut self, g: &Guest, x: f64) -> Result<f64> {
        (self.cosine)(g, x)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The two measured pool values, each as its own eight-byte segment at the address the body
    /// computes.
    ///
    /// One segment per cell rather than one rodata window: a body that read the wrong address gets
    /// an `Err` out of [`Guest`] instead of a plausible number. The words are the ones read out of
    /// `probe/harness/out/image/`, `1e18` included.
    pub fn with_pool_segments(g: &mut Guest) {
        g.put(POOL_STEP_DOWN, 1.0f64.to_bits().to_be_bytes().to_vec());
        g.put(POOL_INTEGRAL_MAGNITUDE, 1e18f64.to_bits().to_be_bytes().to_vec());
    }

    /// A guest map holding the rodata cells `mathlib` reads, and nothing else.
    fn guest() -> Guest {
        let mut g = Guest::default();
        with_pool_segments(&mut g);
        g
    }

    /// A `Trig` that answers with values the test chose, so no test here depends on libm.
    #[derive(Default)]
    pub struct Scripted {
        pub sine: f64,
        pub cosine: f64,
        pub asked: Vec<(char, f64)>,
    }

    impl Trig for Scripted {
        fn sine(&mut self, _g: &Guest, x: f64) -> Result<f64> {
            self.asked.push(('s', x));
            Ok(self.sine)
        }
        fn cosine(&mut self, _g: &Guest, x: f64) -> Result<f64> {
            self.asked.push(('c', x));
            Ok(self.cosine)
        }
    }

    #[test]
    fn it_floors_the_ordinary_range_and_agrees_with_the_library_there() {
        let g = guest();
        for x in [
            0.5, 1.0, 1.5, 2.75, -0.5, -1.0, -1.5, -2.75, 1e15, -1e15, 1e17, -1e17, 4.9, -4.9,
            0.9999999999999999,
        ] {
            assert_eq!(floor(&g, x).unwrap(), x.floor(), "floor({x})");
        }
    }

    #[test]
    fn the_step_down_is_what_makes_it_a_floor_and_not_a_truncation() {
        // The distinguishing input set is the negatives with a fraction: fctidz gives -2 for -2.75
        // and the floor is -3. Reading the cell live is what supplies the 1.0, so a map whose cell
        // holds something else steps by that instead — which is how a patched image reaches this.
        let g = guest();
        assert_eq!(floor(&g, -2.75).unwrap(), -3.0);
        assert_eq!(floor(&g, -0.25).unwrap(), -1.0);
        // Exact integers do not step: the fraction is +0.0, and +0.0 >= 0.0 takes the first arm.
        assert_eq!(floor(&g, -3.0).unwrap(), -3.0);

        let mut patched = guest();
        patched.set_u64(POOL_STEP_DOWN, 10.0f64.to_bits()).unwrap();
        assert_eq!(floor(&patched, -2.75).unwrap(), -12.0, "the cell is read, not folded in");
    }

    #[test]
    fn the_magnitude_cell_is_1e18_and_it_is_the_threshold_that_is_read() {
        // Measured from the image, against a note that predicted 2^52. Pinned here because the
        // difference is observable: at 1e17 the magnitude arm is not taken and the fsel chain
        // computes a floor; the answer is the same either way, so the test has to use the cell.
        let g = guest();
        assert_eq!(g.u64(POOL_INTEGRAL_MAGNITUDE).unwrap(), 0x43AB_C16D_674E_C800);
        assert_eq!(f64::from_bits(0x43AB_C16D_674E_C800), 1e18);

        // Lower the threshold under an input that has a fraction, and the pass-through arm takes
        // over: floor(2.75) stops being 2.0 and becomes 2.75.
        let mut patched = guest();
        patched.set_u64(POOL_INTEGRAL_MAGNITUDE, 1.0f64.to_bits()).unwrap();
        assert_eq!(floor(&patched, 2.75).unwrap(), 2.75, "|x| > cell passes through unchanged");
        assert_eq!(floor(&patched, 0.75).unwrap(), 0.0, "|x| <= cell still floors");
    }

    #[test]
    fn zero_keeps_its_sign_and_nan_comes_back_with_its_payload() {
        let g = guest();
        // -0.0 is the whole reason for the third fsel: trunc(-0.0) is +0.0 through fctidz/fcfid,
        // and without the select the function would lose the sign.
        assert_eq!(floor(&g, -0.0).unwrap().to_bits(), 0x8000_0000_0000_0000);
        assert_eq!(floor(&g, 0.0).unwrap().to_bits(), 0);
        // NaN takes the else arm of every select, so it returns through the `x` path untouched.
        let nan = f64::from_bits(0x7FF8_0000_DEAD_BEEF);
        assert_eq!(floor(&g, nan).unwrap().to_bits(), 0x7FF8_0000_DEAD_BEEF);
        // Infinities are above the magnitude cell, so they pass through as well.
        assert_eq!(floor(&g, f64::INFINITY).unwrap(), f64::INFINITY);
        assert_eq!(floor(&g, f64::NEG_INFINITY).unwrap(), f64::NEG_INFINITY);
    }

    #[test]
    fn it_reads_both_pool_cells_on_every_call() {
        // The cells are read live, so a map without them is an error rather than a guess — and both
        // are read unconditionally, including on the calls whose answer does not depend on either.
        let bare = Guest::default();
        assert!(floor(&bare, 1.5).is_err(), "neither cell mapped");
        let mut only_step = Guest::default();
        only_step.put(POOL_STEP_DOWN, 1.0f64.to_bits().to_be_bytes().to_vec());
        assert!(floor(&only_step, 8.0).is_err(), "the magnitude cell is read even for 8.0");
        let mut only_mag = Guest::default();
        only_mag.put(POOL_INTEGRAL_MAGNITUDE, 1e18f64.to_bits().to_be_bytes().to_vec());
        assert!(floor(&only_mag, 8.0).is_err(), "the step cell is read even with no fraction");
    }

    #[test]
    fn the_unported_trig_refuses_rather_than_answering() {
        let g = guest();
        let mut t = Unported;
        let e = t.sine(&g, 0.5).unwrap_err();
        assert_eq!(e.address, 0x82F4_DED0);
        assert!(e.message.contains("82F4DED0"));
        assert_eq!(t.cosine(&g, 0.5).unwrap_err().address, 0x82F4_DFB0);

        // And the closure adapter passes the argument straight through, in both slots.
        let mut c = Closures::new(|_g: &Guest, x: f64| Ok(x + 1.0), |_g: &Guest, x: f64| Ok(x + 2.0));
        assert_eq!(c.sine(&g, 10.0).unwrap(), 11.0);
        assert_eq!(c.cosine(&g, 10.0).unwrap(), 12.0);
    }

    #[test]
    fn it_restores_the_entry_flush_mode() {
        let g = guest();
        let before = crate::vmx::get_mxcsr();
        floor(&g, -2.75).unwrap();
        assert_eq!(crate::vmx::get_mxcsr(), before);
    }
}
