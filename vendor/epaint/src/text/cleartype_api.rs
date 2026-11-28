use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

static DUAL_SOURCE_SUPPORTED: AtomicBool = AtomicBool::new(false);
static GAMMA_BITS: AtomicU32 = AtomicU32::new(1.8_f32.to_bits());
static ENHANCED_CONTRAST_BITS: AtomicU32 = AtomicU32::new(0.5_f32.to_bits());
static SUBPIXEL_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Called by `egui-wgpu` / `egui_glow` when hardware dual-source blending (`Src1`/`OneMinusSrc1`) is active.
pub fn set_dual_source_blending_supported(supported: bool) {
    DUAL_SOURCE_SUPPORTED.store(supported, Ordering::Release);
}

/// Whether the active GPU backend supports hardware dual-source ClearType blending.
pub fn dual_source_blending_supported() -> bool {
    DUAL_SOURCE_SUPPORTED.load(Ordering::Acquire)
}

/// Called by the DirectWrite rasterizer to publish OS ClearType blend params `(gamma, enhanced_contrast)`
/// and whether RGB subpixel masks are active in the font atlas (`TextureId::Managed(0)`).
pub fn set_cleartype_state(subpixel_active: bool, gamma: f32, enhanced_contrast: f32) {
    GAMMA_BITS.store(gamma.clamp(1.0, 2.6).to_bits(), Ordering::Release);
    ENHANCED_CONTRAST_BITS.store(enhanced_contrast.clamp(0.0, 2.0).to_bits(), Ordering::Release);
    SUBPIXEL_ACTIVE.store(subpixel_active, Ordering::Release);
}

/// Whether `TextureId::Managed(0)` currently stores 3x1 RGB ClearType subpixel masks.
pub fn cleartype_subpixel_active() -> bool {
    SUBPIXEL_ACTIVE.load(Ordering::Acquire)
}

/// Returns `(gamma, enhanced_contrast)` for ClearType luminance-aware blending in shaders.
pub fn cleartype_blend_params() -> (f32, f32) {
    (
        f32::from_bits(GAMMA_BITS.load(Ordering::Acquire)),
        f32::from_bits(ENHANCED_CONTRAST_BITS.load(Ordering::Acquire)),
    )
}
