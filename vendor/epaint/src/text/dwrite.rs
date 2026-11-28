//! Windows DirectWrite TrueType-hinted ClearType 3x1 RGB subpixel rasterizer.
//!
//! Links directly against system `dwrite.dll` with zero external crate dependencies,
//! serving in-memory font bytes (both Windows system fonts and bundled subsets) through
//! a single process-wide `IDWriteFontFileLoader` + leak-free `Weak`-indexed registry.

#![allow(unsafe_code)]

use crate::text::FontData;

/// Effective text rasterization backend selected for a [`crate::text::FontsImpl`] instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextBackendMode {
    /// DirectWrite TrueType-hinted 3x1 RGB subpixel masks + hardware dual-source blending.
    ClearTypeSubpixel,
    /// DirectWrite TrueType-hinted grayscale coverage (used when GPU lacks dual-source,
    /// monitor geometry is `FLAT`, or `CELLAN_TEXT_BACKEND=grayscale`).
    DirectWriteGrayscale,
    /// Original `ab_glyph` CPU rasterizer (`CELLAN_TEXT_BACKEND=ab_glyph` or non-Windows).
    AbGlyph,
}

/// Output pixel buffer for a single DirectWrite-rasterized glyph.
pub(crate) enum GlyphPixels {
    /// 3x1 RGB subpixel coverage `[r, g, b]` in `0..=255` (already BGR-swapped if needed).
    SubpixelRgb(Vec<[u8; 3]>),
    /// Linear grayscale coverage in `0.0..=1.0`.
    Grayscale(Vec<f32>),
}

/// Rasterized glyph metrics & bitmap in physical pixels relative to baseline `(0, 0)`.
pub(crate) struct RasterizedGlyph {
    pub left: i32,
    pub top: i32,
    pub width: usize,
    pub height: usize,
    /// Horizontal advance in physical pixels (already GDI-snapped or rounded when appropriate).
    pub advance_px: f32,
    pub pixels: Option<GlyphPixels>,
}

/// Parse `CELLAN_TEXT_BACKEND` env override:
/// - `"ab_glyph"` -> force original `ab_glyph`
/// - `"grayscale"` -> force DirectWrite hinted grayscale
/// - `"cleartype"` / unset / anything else -> ClearType RGB when supported, else DirectWrite grayscale
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EnvBackendPref {
    ClearType,
    Grayscale,
    AbGlyph,
}

fn env_backend_pref() -> EnvBackendPref {
    match std::env::var("CELLAN_TEXT_BACKEND") {
        Ok(val) => {
            let trimmed = val.trim();
            if trimmed.eq_ignore_ascii_case("ab_glyph") || trimmed.eq_ignore_ascii_case("abglyph") {
                EnvBackendPref::AbGlyph
            } else if trimmed.eq_ignore_ascii_case("grayscale")
                || trimmed.eq_ignore_ascii_case("gray")
            {
                EnvBackendPref::Grayscale
            } else {
                EnvBackendPref::ClearType
            }
        }
        Err(_) => EnvBackendPref::ClearType,
    }
}

#[cfg(windows)]
pub(crate) use win_impl::{current_backend_mode, publish_cleartype_state, DWriteFace};

#[cfg(not(windows))]
mod stub_impl {
    use super::*;
    use std::sync::Arc;

    #[derive(Clone, Copy, Debug)]
    pub(crate) struct DWriteParams {
        pub gamma: f32,
        pub enhanced_contrast: f32,
    }

    pub(crate) struct DWriteFace;

    impl DWriteFace {
        pub(crate) fn from_font_data(_data: &FontData) -> Option<Arc<Self>> {
            None
        }

        pub(crate) fn rasterize_glyph(
            &self,
            _glyph_id: u16,
            _em_size_dips: f32,
            _pixels_per_point: f32,
            _subpixel: bool,
        ) -> Option<RasterizedGlyph> {
            None
        }
    }

    pub(crate) fn current_backend_mode() -> TextBackendMode {
        TextBackendMode::AbGlyph
    }

    pub(crate) fn publish_cleartype_state(_mode: TextBackendMode) -> DWriteParams {
        crate::text::set_cleartype_state(false, 1.8, 0.5);
        DWriteParams {
            gamma: 1.8,
            enhanced_contrast: 0.5,
        }
    }
}

#[cfg(not(windows))]
pub(crate) use stub_impl::{current_backend_mode, publish_cleartype_state, DWriteFace};

// ============================================================================
// Windows DirectWrite COM implementation
// ============================================================================

#[cfg(windows)]
mod win_impl {
    #![allow(non_snake_case, non_camel_case_types, dead_code)]

    use std::borrow::Cow;
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::ptr::{self, NonNull};
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
    use std::sync::{Arc, OnceLock, RwLock, Weak};

    use super::*;

    // ---- Win32 / COM primitives ----

    type HRESULT = i32;
    type BOOL = i32;

    const S_OK: HRESULT = 0;
    const E_NOTIMPL: HRESULT = 0x8000_4001_u32 as i32;
    const E_NOINTERFACE: HRESULT = 0x8000_4002_u32 as i32;
    const E_POINTER: HRESULT = 0x8000_4003_u32 as i32;
    const E_FAIL: HRESULT = 0x8000_4005_u32 as i32;
    const E_INVALIDARG: HRESULT = 0x8007_0057_u32 as i32;

    #[inline]
    fn succeeded(hr: HRESULT) -> bool {
        hr >= 0
    }

    #[repr(C)]
    #[derive(Clone, Copy, PartialEq, Eq)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    // Verified against Windows SDK 10.0.26100.0 um/dwrite.h & unknwnbase.h
    const IID_IUNKNOWN: Guid = Guid {
        data1: 0x0000_0000,
        data2: 0x0000,
        data3: 0x0000,
        data4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
    };
    const IID_IDWRITE_FACTORY: Guid = Guid {
        data1: 0xb859_ee5a,
        data2: 0xd838,
        data3: 0x4b5b,
        data4: [0xa2, 0xe8, 0x1a, 0xdc, 0x7d, 0x93, 0xdb, 0x48],
    };
    const IID_IDWRITE_FONT_FILE_LOADER: Guid = Guid {
        data1: 0x727c_ad4e,
        data2: 0xd6af,
        data3: 0x4c9e,
        data4: [0x8a, 0x08, 0xd6, 0x95, 0xb1, 0x1c, 0xaa, 0x49],
    };
    const IID_IDWRITE_FONT_FILE_STREAM: Guid = Guid {
        data1: 0x6d48_65fe,
        data2: 0x0ab8,
        data3: 0x4d91,
        data4: [0x8f, 0x62, 0x5d, 0xd6, 0xbe, 0x34, 0xa3, 0xe0],
    };

    // ---- DirectWrite enums & structs (verified against dwrite.h & dcommon.h) ----

    const DWRITE_FACTORY_TYPE_SHARED: u32 = 0;

    const DWRITE_FONT_FACE_TYPE_CFF: u32 = 0;
    const DWRITE_FONT_FACE_TYPE_TRUETYPE: u32 = 1;
    const DWRITE_FONT_FACE_TYPE_OPENTYPE_COLLECTION: u32 = 2;

    const DWRITE_FONT_SIMULATIONS_NONE: u32 = 0;

    const DWRITE_PIXEL_GEOMETRY_FLAT: u32 = 0;
    const DWRITE_PIXEL_GEOMETRY_RGB: u32 = 1;
    const DWRITE_PIXEL_GEOMETRY_BGR: u32 = 2;

    const DWRITE_RENDERING_MODE_DEFAULT: u32 = 0;
    const DWRITE_RENDERING_MODE_ALIASED: u32 = 1;
    const DWRITE_RENDERING_MODE_GDI_CLASSIC: u32 = 2;
    const DWRITE_RENDERING_MODE_GDI_NATURAL: u32 = 3;
    const DWRITE_RENDERING_MODE_NATURAL: u32 = 4;
    const DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC: u32 = 5;
    const DWRITE_RENDERING_MODE_OUTLINE: u32 = 6;

    const DWRITE_MEASURING_MODE_NATURAL: u32 = 0;
    const DWRITE_MEASURING_MODE_GDI_CLASSIC: u32 = 1;
    const DWRITE_MEASURING_MODE_GDI_NATURAL: u32 = 2;

    const DWRITE_TEXTURE_ALIASED_1X1: u32 = 0;
    const DWRITE_TEXTURE_CLEARTYPE_3X1: u32 = 1;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct WinRect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct DWriteMatrix {
        m11: f32,
        m12: f32,
        m21: f32,
        m22: f32,
        dx: f32,
        dy: f32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct DWriteFontMetrics {
        designUnitsPerEm: u16,
        ascent: u16,
        descent: u16,
        lineGap: i16,
        capHeight: u16,
        xHeight: u16,
        underlinePosition: i16,
        underlineThickness: u16,
        strikethroughPosition: i16,
        strikethroughThickness: u16,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct DWriteGlyphMetrics {
        leftSideBearing: i32,
        advanceWidth: u32,
        rightSideBearing: i32,
        topSideBearing: i32,
        advanceHeight: u32,
        bottomSideBearing: i32,
        verticalOriginY: i32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct DWriteGlyphOffset {
        advanceOffset: f32,
        ascenderOffset: f32,
    }

    #[repr(C)]
    struct DWriteGlyphRun {
        fontFace: *mut IDWriteFontFace,
        fontEmSize: f32,
        glyphCount: u32,
        glyphIndices: *const u16,
        glyphAdvances: *const f32,
        glyphOffsets: *const DWriteGlyphOffset,
        isSideways: BOOL,
        bidiLevel: u32,
    }

    // ---- COM VTables (exact slot order verified against dwrite.h) ----

    #[repr(C)]
    struct IUnknownVtbl {
        QueryInterface: unsafe extern "system" fn(
            this: *mut c_void,
            riid: *const Guid,
            ppv: *mut *mut c_void,
        ) -> HRESULT,
        AddRef: unsafe extern "system" fn(this: *mut c_void) -> u32,
        Release: unsafe extern "system" fn(this: *mut c_void) -> u32,
    }

    #[repr(C)]
    struct IDWriteFontFileLoader {
        lpVtbl: *const IDWriteFontFileLoaderVtbl,
    }

    #[repr(C)]
    struct IDWriteFontFileLoaderVtbl {
        parent: IUnknownVtbl,
        CreateStreamFromKey: unsafe extern "system" fn(
            this: *mut c_void,
            fontFileReferenceKey: *const c_void,
            fontFileReferenceKeySize: u32,
            fontFileStream: *mut *mut IDWriteFontFileStream,
        ) -> HRESULT,
    }

    #[repr(C)]
    struct IDWriteFontFileStream {
        lpVtbl: *const IDWriteFontFileStreamVtbl,
    }

    #[repr(C)]
    struct IDWriteFontFileStreamVtbl {
        parent: IUnknownVtbl,
        ReadFileFragment: unsafe extern "system" fn(
            this: *mut c_void,
            fragmentStart: *mut *const c_void,
            fileOffset: u64,
            fragmentSize: u64,
            fragmentContext: *mut *mut c_void,
        ) -> HRESULT,
        ReleaseFileFragment: unsafe extern "system" fn(
            this: *mut c_void,
            fragmentContext: *mut c_void,
        ),
        GetFileSize: unsafe extern "system" fn(this: *mut c_void, fileSize: *mut u64) -> HRESULT,
        GetLastWriteTime:
            unsafe extern "system" fn(this: *mut c_void, lastWriteTime: *mut u64) -> HRESULT,
    }

    #[repr(C)]
    struct IDWriteFontFile {
        lpVtbl: *const IDWriteFontFileVtbl,
    }

    #[repr(C)]
    struct IDWriteFontFileVtbl {
        parent: IUnknownVtbl,
        GetReferenceKey: unsafe extern "system" fn(
            this: *mut IDWriteFontFile,
            key: *mut *const c_void,
            keySize: *mut u32,
        ) -> HRESULT,
        GetLoader: unsafe extern "system" fn(
            this: *mut IDWriteFontFile,
            loader: *mut *mut IDWriteFontFileLoader,
        ) -> HRESULT,
        Analyze: unsafe extern "system" fn(
            this: *mut IDWriteFontFile,
            isSupportedFontType: *mut BOOL,
            fontFileType: *mut u32,
            fontFaceType: *mut u32,
            numberOfFaces: *mut u32,
        ) -> HRESULT,
    }

    #[repr(C)]
    struct IDWriteRenderingParams {
        lpVtbl: *const IDWriteRenderingParamsVtbl,
    }

    #[repr(C)]
    struct IDWriteRenderingParamsVtbl {
        parent: IUnknownVtbl,
        GetGamma: unsafe extern "system" fn(this: *mut IDWriteRenderingParams) -> f32,
        GetEnhancedContrast: unsafe extern "system" fn(this: *mut IDWriteRenderingParams) -> f32,
        GetClearTypeLevel: unsafe extern "system" fn(this: *mut IDWriteRenderingParams) -> f32,
        GetPixelGeometry: unsafe extern "system" fn(this: *mut IDWriteRenderingParams) -> u32,
        GetRenderingMode: unsafe extern "system" fn(this: *mut IDWriteRenderingParams) -> u32,
    }

    #[repr(C)]
    struct IDWriteFontFace {
        lpVtbl: *const IDWriteFontFaceVtbl,
    }

    #[repr(C)]
    struct IDWriteFontFaceVtbl {
        parent: IUnknownVtbl,
        GetType: unsafe extern "system" fn(this: *mut IDWriteFontFace) -> u32,
        GetFiles: unsafe extern "system" fn(
            this: *mut IDWriteFontFace,
            numberOfFiles: *mut u32,
            fontFiles: *mut *mut IDWriteFontFile,
        ) -> HRESULT,
        GetIndex: unsafe extern "system" fn(this: *mut IDWriteFontFace) -> u32,
        GetSimulations: unsafe extern "system" fn(this: *mut IDWriteFontFace) -> u32,
        IsSymbolFont: unsafe extern "system" fn(this: *mut IDWriteFontFace) -> BOOL,
        GetMetrics: unsafe extern "system" fn(
            this: *mut IDWriteFontFace,
            fontFaceMetrics: *mut DWriteFontMetrics,
        ),
        GetGlyphCount: unsafe extern "system" fn(this: *mut IDWriteFontFace) -> u16,
        GetDesignGlyphMetrics: unsafe extern "system" fn(
            this: *mut IDWriteFontFace,
            glyphIndices: *const u16,
            glyphCount: u32,
            glyphMetrics: *mut DWriteGlyphMetrics,
            isSideways: BOOL,
        ) -> HRESULT,
        GetGlyphIndices: unsafe extern "system" fn(
            this: *mut IDWriteFontFace,
            codePoints: *const u32,
            codePointCount: u32,
            glyphIndices: *mut u16,
        ) -> HRESULT,
        TryGetFontTable: unsafe extern "system" fn(
            this: *mut IDWriteFontFace,
            openTypeTableTag: u32,
            tableData: *mut *const c_void,
            tableSize: *mut u32,
            tableContext: *mut *mut c_void,
            exists: *mut BOOL,
        ) -> HRESULT,
        ReleaseFontTable:
            unsafe extern "system" fn(this: *mut IDWriteFontFace, tableContext: *mut c_void),
        GetGlyphRunOutline: unsafe extern "system" fn(
            this: *mut IDWriteFontFace,
            emSize: f32,
            glyphIndices: *const u16,
            glyphAdvances: *const f32,
            glyphOffsets: *const DWriteGlyphOffset,
            glyphCount: u32,
            isSideways: BOOL,
            isRightToLeft: BOOL,
            geometrySink: *mut c_void,
        ) -> HRESULT,
        GetRecommendedRenderingMode: unsafe extern "system" fn(
            this: *mut IDWriteFontFace,
            emSize: f32,
            pixelsPerDip: f32,
            measuringMode: u32,
            renderingParams: *mut IDWriteRenderingParams,
            renderingMode: *mut u32,
        ) -> HRESULT,
        GetGdiCompatibleMetrics: unsafe extern "system" fn(
            this: *mut IDWriteFontFace,
            emSize: f32,
            pixelsPerDip: f32,
            transform: *const DWriteMatrix,
            fontFaceMetrics: *mut DWriteFontMetrics,
        ) -> HRESULT,
        GetGdiCompatibleGlyphMetrics: unsafe extern "system" fn(
            this: *mut IDWriteFontFace,
            emSize: f32,
            pixelsPerDip: f32,
            transform: *const DWriteMatrix,
            useGdiNatural: BOOL,
            glyphIndices: *const u16,
            glyphCount: u32,
            glyphMetrics: *mut DWriteGlyphMetrics,
            isSideways: BOOL,
        ) -> HRESULT,
    }

    #[repr(C)]
    struct IDWriteGlyphRunAnalysis {
        lpVtbl: *const IDWriteGlyphRunAnalysisVtbl,
    }

    #[repr(C)]
    struct IDWriteGlyphRunAnalysisVtbl {
        parent: IUnknownVtbl,
        GetAlphaTextureBounds: unsafe extern "system" fn(
            this: *mut IDWriteGlyphRunAnalysis,
            textureType: u32,
            textureBounds: *mut WinRect,
        ) -> HRESULT,
        CreateAlphaTexture: unsafe extern "system" fn(
            this: *mut IDWriteGlyphRunAnalysis,
            textureType: u32,
            textureBounds: *const WinRect,
            alphaValues: *mut u8,
            bufferSize: u32,
        ) -> HRESULT,
        GetAlphaBlendParams: unsafe extern "system" fn(
            this: *mut IDWriteGlyphRunAnalysis,
            renderingParams: *mut IDWriteRenderingParams,
            blendGamma: *mut f32,
            blendEnhancedContrast: *mut f32,
            blendClearTypeLevel: *mut f32,
        ) -> HRESULT,
    }

    #[repr(C)]
    struct IDWriteFactory {
        lpVtbl: *const IDWriteFactoryVtbl,
    }

    #[repr(C)]
    struct IDWriteFactoryVtbl {
        parent: IUnknownVtbl,
        GetSystemFontCollection: *const c_void,      // slot 3
        CreateCustomFontCollection: *const c_void,   // slot 4
        RegisterFontCollectionLoader: *const c_void, // slot 5
        UnregisterFontCollectionLoader: *const c_void, // slot 6
        CreateFontFileReference: *const c_void,      // slot 7
        CreateCustomFontFileReference: unsafe extern "system" fn(
            this: *mut IDWriteFactory,
            fontFileReferenceKey: *const c_void,
            fontFileReferenceKeySize: u32,
            fontFileLoader: *mut IDWriteFontFileLoader,
            fontFile: *mut *mut IDWriteFontFile,
        ) -> HRESULT, // slot 8
        CreateFontFace: unsafe extern "system" fn(
            this: *mut IDWriteFactory,
            fontFaceType: u32,
            numberOfFiles: u32,
            fontFiles: *const *mut IDWriteFontFile,
            faceIndex: u32,
            fontFaceSimulationFlags: u32,
            fontFace: *mut *mut IDWriteFontFace,
        ) -> HRESULT, // slot 9
        CreateRenderingParams: unsafe extern "system" fn(
            this: *mut IDWriteFactory,
            renderingParams: *mut *mut IDWriteRenderingParams,
        ) -> HRESULT, // slot 10
        CreateMonitorRenderingParams: *const c_void, // slot 11
        CreateCustomRenderingParams: *const c_void,  // slot 12
        RegisterFontFileLoader: unsafe extern "system" fn(
            this: *mut IDWriteFactory,
            fontFileLoader: *mut IDWriteFontFileLoader,
        ) -> HRESULT, // slot 13
        UnregisterFontFileLoader: unsafe extern "system" fn(
            this: *mut IDWriteFactory,
            fontFileLoader: *mut IDWriteFontFileLoader,
        ) -> HRESULT, // slot 14
        CreateTextFormat: *const c_void,             // slot 15
        CreateTypography: *const c_void,             // slot 16
        GetGdiInterop: *const c_void,                // slot 17
        CreateTextLayout: *const c_void,             // slot 18
        CreateGdiCompatibleTextLayout: *const c_void, // slot 19
        CreateEllipsisTrimmingSign: *const c_void,   // slot 20
        CreateTextAnalyzer: *const c_void,           // slot 21
        CreateNumberSubstitution: *const c_void,     // slot 22
        CreateGlyphRunAnalysis: unsafe extern "system" fn(
            this: *mut IDWriteFactory,
            glyphRun: *const DWriteGlyphRun,
            pixelsPerDip: f32,
            transform: *const DWriteMatrix,
            renderingMode: u32,
            measuringMode: u32,
            baselineOriginX: f32,
            baselineOriginY: f32,
            glyphRunAnalysis: *mut *mut IDWriteGlyphRunAnalysis,
        ) -> HRESULT, // slot 23
    }

    #[link(name = "dwrite")]
    extern "system" {
        fn DWriteCreateFactory(
            factoryType: u32,
            iid: *const Guid,
            factory: *mut *mut c_void,
        ) -> HRESULT;
    }

    // ---- RAII COM pointer release helper ----

    unsafe fn com_release(ptr: *mut c_void) {
        if !ptr.is_null() {
            // SAFETY: Every COM interface begins with a pointer to an IUnknown-compatible vtable.
            let vtbl = unsafe { *(ptr as *const *const IUnknownVtbl) };
            // SAFETY: Calling IUnknown::Release on a live COM interface pointer.
            unsafe { ((*vtbl).Release)(ptr) };
        }
    }

    // ---- In-memory FontBytes registry (leak-free Weak table) ----

    enum FontBytes {
        Static(&'static [u8]),
        Shared(Arc<[u8]>),
    }

    impl FontBytes {
        #[inline]
        fn as_slice(&self) -> &[u8] {
            match self {
                Self::Static(s) => s,
                Self::Shared(a) => a,
            }
        }
    }

    static NEXT_FONT_KEY: AtomicU64 = AtomicU64::new(1);

    fn font_registry() -> &'static RwLock<HashMap<u64, Weak<FontBytes>>> {
        static REGISTRY: OnceLock<RwLock<HashMap<u64, Weak<FontBytes>>>> = OnceLock::new();
        REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
    }

    fn register_font_bytes(bytes: &Arc<FontBytes>) -> u64 {
        let key = NEXT_FONT_KEY.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut map) = font_registry().write() {
            // Opportunistically sweep dead weak entries if the map ever grows large.
            if map.len() > 256 {
                map.retain(|_, w| w.strong_count() > 0);
            }
            map.insert(key, Arc::downgrade(bytes));
        }
        key
    }

    fn lookup_font_bytes(key: u64) -> Option<Arc<FontBytes>> {
        font_registry()
            .read()
            .ok()
            .and_then(|map| map.get(&key).and_then(Weak::upgrade))
    }

    fn unregister_font_bytes(key: u64) {
        if let Ok(mut map) = font_registry().write() {
            map.remove(&key);
        }
    }

    // ---- Custom COM IDWriteFontFileStream ----

    #[repr(C)]
    struct MemoryFontFileStream {
        lpVtbl: *const IDWriteFontFileStreamVtbl,
        ref_count: AtomicU32,
        bytes: Arc<FontBytes>,
    }

    static MEMORY_STREAM_VTBL: IDWriteFontFileStreamVtbl = IDWriteFontFileStreamVtbl {
        parent: IUnknownVtbl {
            QueryInterface: stream_query_interface,
            AddRef: stream_add_ref,
            Release: stream_release,
        },
        ReadFileFragment: stream_read_file_fragment,
        ReleaseFileFragment: stream_release_file_fragment,
        GetFileSize: stream_get_file_size,
        GetLastWriteTime: stream_get_last_write_time,
    };

    unsafe extern "system" fn stream_query_interface(
        this: *mut c_void,
        riid: *const Guid,
        ppv: *mut *mut c_void,
    ) -> HRESULT {
        if ppv.is_null() || riid.is_null() {
            return E_POINTER;
        }
        // SAFETY: Caller passes valid GUID pointer and out-pointer per COM contract.
        let iid = unsafe { *riid };
        if iid == IID_IUNKNOWN || iid == IID_IDWRITE_FONT_FILE_STREAM {
            // SAFETY: Valid COM out-pointer.
            unsafe { *ppv = this };
            // SAFETY: Valid stream pointer.
            unsafe { stream_add_ref(this) };
            S_OK
        } else {
            // SAFETY: Valid COM out-pointer.
            unsafe { *ppv = ptr::null_mut() };
            E_NOINTERFACE
        }
    }

    unsafe extern "system" fn stream_add_ref(this: *mut c_void) -> u32 {
        // SAFETY: `this` is a live `MemoryFontFileStream` allocated by `Box::into_raw`.
        let stream = unsafe { &*(this as *const MemoryFontFileStream) };
        stream.ref_count.fetch_add(1, Ordering::Relaxed) + 1
    }

    unsafe extern "system" fn stream_release(this: *mut c_void) -> u32 {
        // SAFETY: `this` is a live `MemoryFontFileStream` allocated by `Box::into_raw`.
        let stream = unsafe { &*(this as *const MemoryFontFileStream) };
        let prev = stream.ref_count.fetch_sub(1, Ordering::Release);
        let next = prev.saturating_sub(1);
        if next == 0 {
            std::sync::atomic::fence(Ordering::Acquire);
            // SAFETY: Refcount hit 0; we are the sole remaining owner.
            drop(unsafe { Box::from_raw(this as *mut MemoryFontFileStream) });
        }
        next
    }

    unsafe extern "system" fn stream_read_file_fragment(
        this: *mut c_void,
        fragmentStart: *mut *const c_void,
        fileOffset: u64,
        fragmentSize: u64,
        fragmentContext: *mut *mut c_void,
    ) -> HRESULT {
        if fragmentStart.is_null() || fragmentContext.is_null() {
            return E_POINTER;
        }
        // SAFETY: `this` is a live `MemoryFontFileStream`.
        let stream = unsafe { &*(this as *const MemoryFontFileStream) };
        let slice = stream.bytes.as_slice();
        let len = slice.len() as u64;
        let Some(end) = fileOffset.checked_add(fragmentSize) else {
            // SAFETY: Valid out-pointers.
            unsafe {
                *fragmentStart = ptr::null();
                *fragmentContext = ptr::null_mut();
            }
            return E_FAIL;
        };
        if end > len {
            // SAFETY: Valid out-pointers.
            unsafe {
                *fragmentStart = ptr::null();
                *fragmentContext = ptr::null_mut();
            }
            return E_FAIL;
        }
        // SAFETY: `fileOffset <= end <= slice.len()`, and `stream.bytes` stays alive for
        // the entire lifetime of `MemoryFontFileStream` (which outlives every fragment).
        unsafe {
            *fragmentStart = slice.as_ptr().add(fileOffset as usize) as *const c_void;
            *fragmentContext = ptr::null_mut();
        }
        S_OK
    }

    unsafe extern "system" fn stream_release_file_fragment(
        _this: *mut c_void,
        _fragmentContext: *mut c_void,
    ) {
        // No per-fragment allocation; bytes are owned by the stream's Arc<FontBytes>.
    }

    unsafe extern "system" fn stream_get_file_size(
        this: *mut c_void,
        fileSize: *mut u64,
    ) -> HRESULT {
        if fileSize.is_null() {
            return E_POINTER;
        }
        // SAFETY: `this` is a live `MemoryFontFileStream` and `fileSize` is non-null.
        let stream = unsafe { &*(this as *const MemoryFontFileStream) };
        // SAFETY: Caller provides valid out-pointer.
        unsafe { *fileSize = stream.bytes.as_slice().len() as u64 };
        S_OK
    }

    unsafe extern "system" fn stream_get_last_write_time(
        _this: *mut c_void,
        lastWriteTime: *mut u64,
    ) -> HRESULT {
        if !lastWriteTime.is_null() {
            // SAFETY: Caller provides valid out-pointer.
            unsafe { *lastWriteTime = 0 };
        }
        // Per DirectWrite spec, in-memory streams return E_NOTIMPL for last write time.
        E_NOTIMPL
    }

    // ---- Custom COM IDWriteFontFileLoader (immortal process-wide singleton) ----

    #[repr(C)]
    struct MemoryFontFileLoader {
        lpVtbl: *const IDWriteFontFileLoaderVtbl,
    }

    // SAFETY: `MemoryFontFileLoader` contains only a static vtable pointer and no mutable state.
    unsafe impl Sync for MemoryFontFileLoader {}

    static MEMORY_LOADER_VTBL: IDWriteFontFileLoaderVtbl = IDWriteFontFileLoaderVtbl {
        parent: IUnknownVtbl {
            QueryInterface: loader_query_interface,
            AddRef: loader_add_ref,
            Release: loader_release,
        },
        CreateStreamFromKey: loader_create_stream_from_key,
    };

    static MEMORY_LOADER_INSTANCE: MemoryFontFileLoader = MemoryFontFileLoader {
        lpVtbl: &MEMORY_LOADER_VTBL,
    };

    unsafe extern "system" fn loader_query_interface(
        this: *mut c_void,
        riid: *const Guid,
        ppv: *mut *mut c_void,
    ) -> HRESULT {
        if ppv.is_null() || riid.is_null() {
            return E_POINTER;
        }
        // SAFETY: Caller passes valid GUID pointer and out-pointer per COM contract.
        let iid = unsafe { *riid };
        if iid == IID_IUNKNOWN || iid == IID_IDWRITE_FONT_FILE_LOADER {
            // SAFETY: Valid COM out-pointer.
            unsafe { *ppv = this };
            S_OK
        } else {
            // SAFETY: Valid COM out-pointer.
            unsafe { *ppv = ptr::null_mut() };
            E_NOINTERFACE
        }
    }

    unsafe extern "system" fn loader_add_ref(_this: *mut c_void) -> u32 {
        // Static singleton lives for the lifetime of the process.
        1
    }

    unsafe extern "system" fn loader_release(_this: *mut c_void) -> u32 {
        // Static singleton lives for the lifetime of the process.
        1
    }

    unsafe extern "system" fn loader_create_stream_from_key(
        _this: *mut c_void,
        fontFileReferenceKey: *const c_void,
        fontFileReferenceKeySize: u32,
        fontFileStream: *mut *mut IDWriteFontFileStream,
    ) -> HRESULT {
        if fontFileStream.is_null() {
            return E_POINTER;
        }
        // SAFETY: Valid out-pointer.
        unsafe { *fontFileStream = ptr::null_mut() };
        if fontFileReferenceKey.is_null()
            || fontFileReferenceKeySize as usize != std::mem::size_of::<u64>()
        {
            return E_INVALIDARG;
        }
        // SAFETY: Verified non-null and exact 8-byte key size.
        let key = unsafe { ptr::read_unaligned(fontFileReferenceKey as *const u64) };
        let Some(bytes) = lookup_font_bytes(key) else {
            return E_FAIL;
        };
        let stream = Box::new(MemoryFontFileStream {
            lpVtbl: &MEMORY_STREAM_VTBL,
            ref_count: AtomicU32::new(1),
            bytes,
        });
        // SAFETY: Ownership transfers to DirectWrite caller (refcount == 1).
        unsafe { *fontFileStream = Box::into_raw(stream) as *mut IDWriteFontFileStream };
        S_OK
    }

    // ---- Process-wide DirectWrite Factory & OS ClearType parameters ----

    #[derive(Clone, Copy, Debug)]
    pub(crate) struct DWriteParams {
        pub gamma: f32,
        pub enhanced_contrast: f32,
        pub clear_type_level: f32,
        pub pixel_geometry: u32,
    }

    struct DWriteSystem {
        factory: NonNull<IDWriteFactory>,
        rendering_params: NonNull<IDWriteRenderingParams>,
        loader: *mut IDWriteFontFileLoader,
        params: DWriteParams,
    }

    // SAFETY: Microsoft DirectWrite documents `IDWriteFactory`, `IDWriteRenderingParams`,
    // and stateless `IDWriteFontFileLoader` as agile / free-threaded COM objects.
    unsafe impl Send for DWriteSystem {}
    // SAFETY: Same as above.
    unsafe impl Sync for DWriteSystem {}

    fn dwrite_system() -> Option<&'static DWriteSystem> {
        static SYSTEM: OnceLock<Option<DWriteSystem>> = OnceLock::new();
        SYSTEM.get_or_init(init_dwrite_system).as_ref()
    }

    fn init_dwrite_system() -> Option<DWriteSystem> {
        let mut factory_raw: *mut c_void = ptr::null_mut();
        // SAFETY: Valid FFI call into system `dwrite.dll`.
        let hr = unsafe {
            DWriteCreateFactory(
                DWRITE_FACTORY_TYPE_SHARED,
                &IID_IDWRITE_FACTORY,
                &mut factory_raw,
            )
        };
        if !succeeded(hr) || factory_raw.is_null() {
            return None;
        }
        let factory = NonNull::new(factory_raw as *mut IDWriteFactory)?;

        let mut params_raw: *mut IDWriteRenderingParams = ptr::null_mut();
        // SAFETY: `factory` is a valid live `IDWriteFactory`.
        let hr_params = unsafe {
            let vtbl = (*factory.as_ptr()).lpVtbl;
            ((*vtbl).CreateRenderingParams)(factory.as_ptr(), &mut params_raw)
        };
        if !succeeded(hr_params) || params_raw.is_null() {
            // SAFETY: Release factory on init failure.
            unsafe { com_release(factory.as_ptr() as *mut c_void) };
            return None;
        }
        let rendering_params = NonNull::new(params_raw)?;

        let loader = &MEMORY_LOADER_INSTANCE as *const MemoryFontFileLoader as *mut IDWriteFontFileLoader;
        // SAFETY: Registering our static process-wide `IDWriteFontFileLoader` once.
        let hr_reg = unsafe {
            let vtbl = (*factory.as_ptr()).lpVtbl;
            ((*vtbl).RegisterFontFileLoader)(factory.as_ptr(), loader)
        };
        if !succeeded(hr_reg) {
            // SAFETY: Release COM objects on registration failure.
            unsafe {
                com_release(rendering_params.as_ptr() as *mut c_void);
                com_release(factory.as_ptr() as *mut c_void);
            }
            return None;
        }

        // SAFETY: `rendering_params` is a valid live `IDWriteRenderingParams`.
        let (gamma, enhanced_contrast, clear_type_level, pixel_geometry) = unsafe {
            let vtbl = (*rendering_params.as_ptr()).lpVtbl;
            (
                ((*vtbl).GetGamma)(rendering_params.as_ptr()),
                ((*vtbl).GetEnhancedContrast)(rendering_params.as_ptr()),
                ((*vtbl).GetClearTypeLevel)(rendering_params.as_ptr()),
                ((*vtbl).GetPixelGeometry)(rendering_params.as_ptr()),
            )
        };

        let params = DWriteParams {
            gamma: if gamma.is_finite() && (1.0..=2.6).contains(&gamma) {
                gamma
            } else {
                1.8
            },
            enhanced_contrast: if enhanced_contrast.is_finite()
                && (0.0..=2.0).contains(&enhanced_contrast)
            {
                enhanced_contrast
            } else {
                0.5
            },
            clear_type_level: if clear_type_level.is_finite() {
                clear_type_level.clamp(0.0, 1.0)
            } else {
                1.0
            },
            pixel_geometry,
        };

        Some(DWriteSystem {
            factory,
            rendering_params,
            loader,
            params,
        })
    }

    pub(crate) fn current_backend_mode() -> TextBackendMode {
        let pref = env_backend_pref();
        if pref == EnvBackendPref::AbGlyph {
            return TextBackendMode::AbGlyph;
        }
        let Some(sys) = dwrite_system() else {
            return TextBackendMode::AbGlyph;
        };
        if pref == EnvBackendPref::Grayscale {
            return TextBackendMode::DirectWriteGrayscale;
        }
        if crate::text::dual_source_blending_supported()
            && sys.params.pixel_geometry != DWRITE_PIXEL_GEOMETRY_FLAT
            && sys.params.clear_type_level > 0.0
        {
            TextBackendMode::ClearTypeSubpixel
        } else {
            TextBackendMode::DirectWriteGrayscale
        }
    }

    pub(crate) fn publish_cleartype_state(mode: TextBackendMode) -> DWriteParams {
        let params = dwrite_system().map_or(
            DWriteParams {
                gamma: 1.8,
                enhanced_contrast: 0.5,
                clear_type_level: 1.0,
                pixel_geometry: DWRITE_PIXEL_GEOMETRY_RGB,
            },
            |sys| sys.params,
        );
        let subpixel_active = mode == TextBackendMode::ClearTypeSubpixel;
        crate::text::set_cleartype_state(subpixel_active, params.gamma, params.enhanced_contrast);
        params
    }

    // ---- Per-Font DirectWrite Face ----

    pub(crate) struct DWriteFace {
        face: NonNull<IDWriteFontFace>,
        key: u64,
        _bytes: Arc<FontBytes>,
        design_units_per_em: u16,
        glyph_count: u16,
    }

    // SAFETY: `IDWriteFontFace` is free-threaded per DirectWrite specification, and `_bytes` is `Arc`.
    unsafe impl Send for DWriteFace {}
    // SAFETY: Same as above.
    unsafe impl Sync for DWriteFace {}

    impl Drop for DWriteFace {
        fn drop(&mut self) {
            // Release COM face first so DirectWrite finishes with any internal stream reference
            // before removing the registry entry and dropping `_bytes`.
            // SAFETY: `self.face` is owned by `self` and valid until drop.
            unsafe { com_release(self.face.as_ptr() as *mut c_void) };
            unregister_font_bytes(self.key);
        }
    }

    fn detect_face_type(bytes: &[u8]) -> u32 {
        if bytes.len() >= 4 {
            match &bytes[..4] {
                b"ttcf" => return DWRITE_FONT_FACE_TYPE_OPENTYPE_COLLECTION,
                b"OTTO" => return DWRITE_FONT_FACE_TYPE_CFF,
                _ => {}
            }
        }
        DWRITE_FONT_FACE_TYPE_TRUETYPE
    }

    impl DWriteFace {
        pub(crate) fn from_font_data(data: &FontData) -> Option<Arc<Self>> {
            let sys = dwrite_system()?;
            let slice = data.font.as_ref();
            if slice.len() < 12 {
                return None;
            }
            let font_bytes = match &data.font {
                Cow::Borrowed(s) => Arc::new(FontBytes::Static(s)),
                Cow::Owned(v) => Arc::new(FontBytes::Shared(Arc::from(v.as_slice()))),
            };
            let key = register_font_bytes(&font_bytes);

            let mut font_file: *mut IDWriteFontFile = ptr::null_mut();
            // SAFETY: `sys.factory` and `sys.loader` are live process-wide COM objects;
            // `key` is registered in `font_registry()` and backed by `font_bytes`.
            let hr_file = unsafe {
                let vtbl = (*sys.factory.as_ptr()).lpVtbl;
                ((*vtbl).CreateCustomFontFileReference)(
                    sys.factory.as_ptr(),
                    (&key as *const u64) as *const c_void,
                    std::mem::size_of::<u64>() as u32,
                    sys.loader,
                    &mut font_file,
                )
            };
            if !succeeded(hr_file) || font_file.is_null() {
                unregister_font_bytes(key);
                return None;
            }

            // Validate bytes via IDWriteFontFile::Analyze (gracefully handles corrupt/synthetic bytes
            // and reports the authoritative face type and face count).
            let mut is_supported: BOOL = 0;
            let mut file_type: u32 = 0;
            let mut analyzed_face_type: u32 = detect_face_type(slice);
            let mut num_faces: u32 = 0;
            // SAFETY: `font_file` is a valid live `IDWriteFontFile`.
            let hr_analyze = unsafe {
                let vtbl = (*font_file).lpVtbl;
                ((*vtbl).Analyze)(
                    font_file,
                    &mut is_supported,
                    &mut file_type,
                    &mut analyzed_face_type,
                    &mut num_faces,
                )
            };
            if !succeeded(hr_analyze) || is_supported == 0 || data.index >= num_faces {
                // SAFETY: Release temporary `IDWriteFontFile`.
                unsafe { com_release(font_file as *mut c_void) };
                unregister_font_bytes(key);
                return None;
            }

            let face_type = if analyzed_face_type == DWRITE_FONT_FACE_TYPE_CFF
                || analyzed_face_type == DWRITE_FONT_FACE_TYPE_TRUETYPE
                || analyzed_face_type == DWRITE_FONT_FACE_TYPE_OPENTYPE_COLLECTION
            {
                analyzed_face_type
            } else {
                detect_face_type(slice)
            };

            let files = [font_file];
            let mut face_raw: *mut IDWriteFontFace = ptr::null_mut();
            // SAFETY: `sys.factory` and `files[0]` are valid live COM pointers.
            let hr_face = unsafe {
                let vtbl = (*sys.factory.as_ptr()).lpVtbl;
                ((*vtbl).CreateFontFace)(
                    sys.factory.as_ptr(),
                    face_type,
                    1,
                    files.as_ptr(),
                    data.index,
                    DWRITE_FONT_SIMULATIONS_NONE,
                    &mut face_raw,
                )
            };
            // Release our reference to `font_file`; `IDWriteFontFace` holds its own ref if created.
            // SAFETY: `font_file` is valid and no longer needed locally.
            unsafe { com_release(font_file as *mut c_void) };

            if !succeeded(hr_face) || face_raw.is_null() {
                unregister_font_bytes(key);
                return None;
            }
            let face = NonNull::new(face_raw)?;

            let mut metrics = DWriteFontMetrics::default();
            // SAFETY: `face` is a valid live `IDWriteFontFace`.
            let glyph_count = unsafe {
                let vtbl = (*face.as_ptr()).lpVtbl;
                ((*vtbl).GetMetrics)(face.as_ptr(), &mut metrics);
                ((*vtbl).GetGlyphCount)(face.as_ptr())
            };
            if metrics.designUnitsPerEm == 0 {
                // SAFETY: Release `face` if font metrics are degenerate.
                unsafe { com_release(face.as_ptr() as *mut c_void) };
                unregister_font_bytes(key);
                return None;
            }

            Some(Arc::new(Self {
                face,
                key,
                _bytes: font_bytes,
                design_units_per_em: metrics.designUnitsPerEm,
                glyph_count,
            }))
        }

        /// Choose rendering & measuring mode for knife-edge 100% DPI (Excel/WPS parity)
        /// and smooth HiDPI sharpness.
        fn select_modes(
            &self,
            sys: &DWriteSystem,
            em_size_dips: f32,
            pixels_per_point: f32,
        ) -> (u32, u32) {
            let mut rec_mode = DWRITE_RENDERING_MODE_NATURAL;
            // SAFETY: `self.face` and `sys.rendering_params` are valid live COM pointers.
            let hr = unsafe {
                let vtbl = (*self.face.as_ptr()).lpVtbl;
                ((*vtbl).GetRecommendedRenderingMode)(
                    self.face.as_ptr(),
                    em_size_dips,
                    pixels_per_point,
                    DWRITE_MEASURING_MODE_NATURAL,
                    sys.rendering_params.as_ptr(),
                    &mut rec_mode,
                )
            };
            if !succeeded(hr) {
                rec_mode = DWRITE_RENDERING_MODE_NATURAL;
            }

            let em_size_px = em_size_dips * pixels_per_point;
            if pixels_per_point <= 1.25 && em_size_px <= 20.0 {
                // At 100%–125% DPI for UI & spreadsheet grid sizes, prefer GDI_NATURAL:
                // snaps both horizontal AND vertical stems (crucial for crisp currency digits 0-9 & '.')
                // onto whole integer pixels without splitting 1px vertical stems into red/blue fringes,
                // while preserving natural proportional advance widths.
                match rec_mode {
                    DWRITE_RENDERING_MODE_ALIASED => {
                        (DWRITE_RENDERING_MODE_ALIASED, DWRITE_MEASURING_MODE_GDI_CLASSIC)
                    }
                    DWRITE_RENDERING_MODE_GDI_CLASSIC => (
                        DWRITE_RENDERING_MODE_GDI_CLASSIC,
                        DWRITE_MEASURING_MODE_GDI_CLASSIC,
                    ),
                    _ => (
                        DWRITE_RENDERING_MODE_GDI_NATURAL,
                        DWRITE_MEASURING_MODE_GDI_NATURAL,
                    ),
                }
            } else {
                // At HiDPI (>125%) or display sizes (>22px), honor symmetric 2D AA when recommended,
                // never allowing OUTLINE (which CreateGlyphRunAnalysis rejects).
                match rec_mode {
                    DWRITE_RENDERING_MODE_GDI_CLASSIC => (
                        DWRITE_RENDERING_MODE_GDI_CLASSIC,
                        DWRITE_MEASURING_MODE_GDI_CLASSIC,
                    ),
                    DWRITE_RENDERING_MODE_GDI_NATURAL => (
                        DWRITE_RENDERING_MODE_GDI_NATURAL,
                        DWRITE_MEASURING_MODE_GDI_NATURAL,
                    ),
                    DWRITE_RENDERING_MODE_NATURAL => {
                        (DWRITE_RENDERING_MODE_NATURAL, DWRITE_MEASURING_MODE_NATURAL)
                    }
                    DWRITE_RENDERING_MODE_ALIASED if em_size_px <= 18.0 => {
                        (DWRITE_RENDERING_MODE_ALIASED, DWRITE_MEASURING_MODE_GDI_CLASSIC)
                    }
                    _ => (
                        DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC,
                        DWRITE_MEASURING_MODE_NATURAL,
                    ),
                }
            }
        }

        fn glyph_metrics_px(
            &self,
            glyph_id: u16,
            em_size_dips: f32,
            pixels_per_point: f32,
            measuring_mode: u32,
        ) -> Option<(f32, f32)> {
            let mut gm = DWriteGlyphMetrics::default();
            let hr = match measuring_mode {
                DWRITE_MEASURING_MODE_GDI_CLASSIC | DWRITE_MEASURING_MODE_GDI_NATURAL => {
                    let use_gdi_natural = i32::from(measuring_mode == DWRITE_MEASURING_MODE_GDI_NATURAL);
                    // SAFETY: `self.face` is a valid live `IDWriteFontFace`; single-element pointers are valid.
                    unsafe {
                        let vtbl = (*self.face.as_ptr()).lpVtbl;
                        ((*vtbl).GetGdiCompatibleGlyphMetrics)(
                            self.face.as_ptr(),
                            em_size_dips,
                            pixels_per_point,
                            ptr::null(),
                            use_gdi_natural,
                            &glyph_id,
                            1,
                            &mut gm,
                            0,
                        )
                    }
                }
                _ => {
                    // SAFETY: `self.face` is a valid live `IDWriteFontFace`; single-element pointers are valid.
                    unsafe {
                        let vtbl = (*self.face.as_ptr()).lpVtbl;
                        ((*vtbl).GetDesignGlyphMetrics)(
                            self.face.as_ptr(),
                            &glyph_id,
                            1,
                            &mut gm,
                            0,
                        )
                    }
                }
            };
            if !succeeded(hr) {
                return None;
            }
            let scale = (em_size_dips * pixels_per_point) / (self.design_units_per_em as f32);
            let raw_px = (gm.advanceWidth as f32) * scale;
            let lsb_px = (gm.leftSideBearing as f32) * scale;
            let adv_px = if pixels_per_point <= 1.25
                || measuring_mode == DWRITE_MEASURING_MODE_GDI_CLASSIC
            {
                raw_px.round()
            } else {
                raw_px
            };
            Some((adv_px, lsb_px))
        }

        #[cfg(test)]
        pub(crate) fn glyph_index_for_char(&self, c: char) -> Option<u16> {
            let cp = c as u32;
            let mut gid = 0u16;
            // SAFETY: `self.face` is a valid live `IDWriteFontFace`.
            let hr = unsafe {
                let vtbl = (*self.face.as_ptr()).lpVtbl;
                ((*vtbl).GetGlyphIndices)(self.face.as_ptr(), &cp, 1, &mut gid)
            };
            succeeded(hr).then_some(gid)
        }

        pub(crate) fn rasterize_glyph(
            &self,
            glyph_id: u16,
            em_size_dips: f32,
            pixels_per_point: f32,
            subpixel: bool,
        ) -> Option<RasterizedGlyph> {
            if glyph_id == 0 || glyph_id >= self.glyph_count || em_size_dips <= 0.0 {
                return None;
            }
            // DirectWrite CreateGlyphRunAnalysis caps huge outline glyphs; defer giant sizes to ab_glyph.
            if em_size_dips * pixels_per_point > 240.0 {
                return None;
            }
            let sys = dwrite_system()?;
            let (rendering_mode, measuring_mode) =
                self.select_modes(sys, em_size_dips, pixels_per_point);
            let (advance_px, lsb_px) =
                self.glyph_metrics_px(glyph_id, em_size_dips, pixels_per_point, measuring_mode)?;

            let run = DWriteGlyphRun {
                fontFace: self.face.as_ptr(),
                fontEmSize: em_size_dips,
                glyphCount: 1,
                glyphIndices: &glyph_id,
                glyphAdvances: ptr::null(),
                glyphOffsets: ptr::null(),
                isSideways: 0,
                bidiLevel: 0,
            };

            let mut analysis_raw: *mut IDWriteGlyphRunAnalysis = ptr::null_mut();
            // SAFETY: `sys.factory` and `run` point to valid live data for the duration of the call.
            let hr_ana = unsafe {
                let vtbl = (*sys.factory.as_ptr()).lpVtbl;
                ((*vtbl).CreateGlyphRunAnalysis)(
                    sys.factory.as_ptr(),
                    &run,
                    pixels_per_point,
                    ptr::null(),
                    rendering_mode,
                    measuring_mode,
                    0.0,
                    0.0,
                    &mut analysis_raw,
                )
            };
            if !succeeded(hr_ana) || analysis_raw.is_null() {
                return None;
            }

            // Ensure COM release on every exit path.
            struct AnalysisGuard(*mut IDWriteGlyphRunAnalysis);
            impl Drop for AnalysisGuard {
                fn drop(&mut self) {
                    // SAFETY: `self.0` is an owned non-null `IDWriteGlyphRunAnalysis`.
                    unsafe { com_release(self.0 as *mut c_void) };
                }
            }
            let analysis = AnalysisGuard(analysis_raw);

            let mut use_aliased = rendering_mode == DWRITE_RENDERING_MODE_ALIASED;
            let mut bounds = WinRect::default();

            if !use_aliased {
                // SAFETY: `analysis.0` is live.
                let hr_b = unsafe {
                    let vtbl = (*analysis.0).lpVtbl;
                    ((*vtbl).GetAlphaTextureBounds)(
                        analysis.0,
                        DWRITE_TEXTURE_CLEARTYPE_3X1,
                        &mut bounds,
                    )
                };
                if !succeeded(hr_b) || bounds.right <= bounds.left || bounds.bottom <= bounds.top {
                    // Some GDI Classic embedded-bitmap glyphs (e.g. SimSun 12px) return empty 3x1 bounds
                    // and store their crisp pixel bitmap in DWRITE_TEXTURE_ALIASED_1X1.
                    use_aliased = true;
                }
            }

            if use_aliased {
                bounds = WinRect::default();
                // SAFETY: `analysis.0` is live.
                let hr_b = unsafe {
                    let vtbl = (*analysis.0).lpVtbl;
                    ((*vtbl).GetAlphaTextureBounds)(
                        analysis.0,
                        DWRITE_TEXTURE_ALIASED_1X1,
                        &mut bounds,
                    )
                };
                if !succeeded(hr_b) {
                    return None;
                }
            }

            let width = (bounds.right - bounds.left).max(0) as usize;
            let height = (bounds.bottom - bounds.top).max(0) as usize;
            if width == 0 || height == 0 {
                // Whitespace or empty glyph (e.g. ' ').
                return Some(RasterizedGlyph {
                    left: 0,
                    top: 0,
                    width: 0,
                    height: 0,
                    advance_px,
                    pixels: None,
                });
            }

            let pixel_count = width.checked_mul(height)?;
            let bgr = sys.params.pixel_geometry == DWRITE_PIXEL_GEOMETRY_BGR;

            if use_aliased {
                let mut alpha = vec![0u8; pixel_count];
                // SAFETY: `alpha` has exact length `width * height` for 1x1 aliased texture.
                let hr_tex = unsafe {
                    let vtbl = (*analysis.0).lpVtbl;
                    ((*vtbl).CreateAlphaTexture)(
                        analysis.0,
                        DWRITE_TEXTURE_ALIASED_1X1,
                        &bounds,
                        alpha.as_mut_ptr(),
                        alpha.len() as u32,
                    )
                };
                if !succeeded(hr_tex) {
                    return None;
                }
                // Trim all-zero transparent border rows/columns from aliased bitmap.
                let mut x0 = 0usize;
                let mut x1 = width;
                let mut y0 = 0usize;
                let mut y1 = height;
                while y0 < y1 && alpha[y0 * width..(y0 + 1) * width].iter().all(|&a| a == 0) {
                    y0 += 1;
                }
                while y1 > y0
                    && alpha[(y1 - 1) * width..y1 * width].iter().all(|&a| a == 0)
                {
                    y1 -= 1;
                }
                while x0 < x1 && (y0..y1).all(|y| alpha[y * width + x0] == 0) {
                    x0 += 1;
                }
                while x1 > x0 && (y0..y1).all(|y| alpha[y * width + (x1 - 1)] == 0) {
                    x1 -= 1;
                }
                let tw = x1 - x0;
                let th = y1 - y0;
                if tw == 0 || th == 0 {
                    return Some(RasterizedGlyph {
                        left: 0,
                        top: 0,
                        width: 0,
                        height: 0,
                        advance_px,
                        pixels: None,
                    });
                }
                let pixels = if subpixel {
                    let mut rgb = Vec::with_capacity(tw * th);
                    for y in y0..y1 {
                        for x in x0..x1 {
                            let a = alpha[y * width + x];
                            rgb.push([a, a, a]);
                        }
                    }
                    GlyphPixels::SubpixelRgb(rgb)
                } else {
                    let mut gray = Vec::with_capacity(tw * th);
                    for y in y0..y1 {
                        for x in x0..x1 {
                            gray.push(alpha[y * width + x] as f32 / 255.0);
                        }
                    }
                    GlyphPixels::Grayscale(gray)
                };
                Some(RasterizedGlyph {
                    left: bounds.left + x0 as i32,
                    top: bounds.top + y0 as i32,
                    width: tw,
                    height: th,
                    advance_px,
                    pixels: Some(pixels),
                })
            } else {
                let buf_len = pixel_count.checked_mul(3)?;
                let mut raw_rgb = vec![0u8; buf_len];
                // SAFETY: `raw_rgb` has exact length `width * height * 3` for 3x1 ClearType texture.
                let hr_tex = unsafe {
                    let vtbl = (*analysis.0).lpVtbl;
                    ((*vtbl).CreateAlphaTexture)(
                        analysis.0,
                        DWRITE_TEXTURE_CLEARTYPE_3X1,
                        &bounds,
                        raw_rgb.as_mut_ptr(),
                        raw_rgb.len() as u32,
                    )
                };
                if !succeeded(hr_tex) {
                    return None;
                }

                // 1. Trim all-zero transparent border rows & columns (DirectWrite's 3-tap ClearType
                //    filter always pads GetAlphaTextureBounds horizontally by 1px even when empty).
                let px_rgb = |x: usize, y: usize| -> [u8; 3] {
                    let i = (y * width + x) * 3;
                    if bgr {
                        [raw_rgb[i + 2], raw_rgb[i + 1], raw_rgb[i]]
                    } else {
                        [raw_rgb[i], raw_rgb[i + 1], raw_rgb[i + 2]]
                    }
                };
                let mut x0 = 0usize;
                let mut x1 = width;
                let mut y0 = 0usize;
                let mut y1 = height;
                while y0 < y1 && (0..width).all(|x| px_rgb(x, y0) == [0, 0, 0]) {
                    y0 += 1;
                }
                while y1 > y0 && (0..width).all(|x| px_rgb(x, y1 - 1) == [0, 0, 0]) {
                    y1 -= 1;
                }
                while x0 < x1 && (y0..y1).all(|y| px_rgb(x0, y) == [0, 0, 0]) {
                    x0 += 1;
                }
                while x1 > x0 && (y0..y1).all(|y| px_rgb(x1 - 1, y) == [0, 0, 0]) {
                    x1 -= 1;
                }

                // 2. At HiDPI (>= 135% scaling, e.g. 150%/175%/200%), emit pure neutral luminance
                //    [l, l, l] inside the ClearType pipeline: keeps DirectWrite hinting, 1:1 physical
                //    pixel NEAREST placement, and luminance-aware linear-light GPU gamma with ZERO
                //    RGB color fringing (matching WinUI 3 / Edge / VS Code at HiDPI).
                //    At 100%–125% DPI, apply luminance-preserving chroma damping so whole-pixel stems
                //    stay 100% solid ink while fractional subpixel edges keep horizontal precision
                //    without glaring red/blue color fringes on dense currency columns.
                const HIDPI_NEUTRAL_THRESHOLD: f32 = 1.35;
                const SUBPIXEL_CHROMA_DAMP: f32 = 0.35;
                let neutral_hidpi = subpixel && pixels_per_point >= HIDPI_NEUTRAL_THRESHOLD;
                let use_neutral_fold = !subpixel || neutral_hidpi;

                // 3. For non-overhanging glyphs (design leftSideBearing >= -0.25px), DirectWrite's
                //    3-tap ClearType filter can spill a faint 1px apron into x = -1 (`bounds.left + x0 < 0`),
                //    which pushes `visual_bounding_rect().min.x` 1px outside tight widget clip rects.
                //    - In grayscale / neutral HiDPI mode: trim x < 0 columns and fold residual luma into x = 0.
                //    - In ClearType subpixel mode: trim a pure filter-tail column at x < 0 whose outer
                //      subpixel (R on RGB / B on BGR) is zero (not a real glyph stem).
                let mut folded_left_gray: Option<Vec<f32>> = None;
                if lsb_px >= -0.25 {
                    if use_neutral_fold {
                        let mut fold = vec![0.0_f32; y1.saturating_sub(y0)];
                        let mut trimmed_any = false;
                        while x0 + 1 < x1 && bounds.left + x0 as i32 <= -1 {
                            for (ry, y) in (y0..y1).enumerate() {
                                let [r, g, b] = px_rgb(x0, y);
                                fold[ry] += (0.299 * r as f32
                                    + 0.587 * g as f32
                                    + 0.114 * b as f32)
                                    / 255.0;
                            }
                            x0 += 1;
                            trimmed_any = true;
                        }
                        if trimmed_any {
                            folded_left_gray = Some(fold);
                        }
                    } else {
                        while x0 + 1 < x1 && bounds.left + x0 as i32 <= -1 {
                            let is_filter_tail_only = (y0..y1).all(|y| {
                                let [r, _g, _b] = px_rgb(x0, y);
                                r == 0
                            });
                            if is_filter_tail_only {
                                x0 += 1;
                            } else {
                                break;
                            }
                        }
                    }
                }

                let tw = x1 - x0;
                let th = y1 - y0;
                if tw == 0 || th == 0 {
                    return Some(RasterizedGlyph {
                        left: 0,
                        top: 0,
                        width: 0,
                        height: 0,
                        advance_px,
                        pixels: None,
                    });
                }

                let pixels = if subpixel {
                    let mut rgb = Vec::with_capacity(tw * th);
                    if neutral_hidpi {
                        for (ry, y) in (y0..y1).enumerate() {
                            for (rx, x) in (x0..x1).enumerate() {
                                let [r, g, b] = px_rgb(x, y);
                                let mut cov = (0.299 * r as f32
                                    + 0.587 * g as f32
                                    + 0.114 * b as f32)
                                    / 255.0;
                                if rx == 0 {
                                    if let Some(fold) = &folded_left_gray {
                                        cov += fold[ry];
                                    }
                                }
                                let l = (cov.clamp(0.0, 1.0) * 255.0).round() as u8;
                                rgb.push([l, l, l]);
                            }
                        }
                    } else {
                        let chroma =
                            SUBPIXEL_CHROMA_DAMP * sys.params.clear_type_level.clamp(0.0, 1.0);
                        for y in y0..y1 {
                            for x in x0..x1 {
                                let [r, g, b] = px_rgb(x, y);
                                let l = 0.299 * (r as f32)
                                    + 0.587 * (g as f32)
                                    + 0.114 * (b as f32);
                                let dr = (l + (r as f32 - l) * chroma)
                                    .round()
                                    .clamp(0.0, 255.0) as u8;
                                let dg = (l + (g as f32 - l) * chroma)
                                    .round()
                                    .clamp(0.0, 255.0) as u8;
                                let db = (l + (b as f32 - l) * chroma)
                                    .round()
                                    .clamp(0.0, 255.0) as u8;
                                rgb.push([dr, dg, db]);
                            }
                        }
                    }
                    GlyphPixels::SubpixelRgb(rgb)
                } else {
                    let mut gray = Vec::with_capacity(tw * th);
                    for (ry, y) in (y0..y1).enumerate() {
                        for (rx, x) in (x0..x1).enumerate() {
                            let [r, g, b] = px_rgb(x, y);
                            let mut cov = (0.299 * r as f32
                                + 0.587 * g as f32
                                + 0.114 * b as f32)
                                / 255.0;
                            if rx == 0 {
                                if let Some(fold) = &folded_left_gray {
                                    cov += fold[ry];
                                }
                            }
                            gray.push(cov.clamp(0.0, 1.0));
                        }
                    }
                    GlyphPixels::Grayscale(gray)
                };
                Some(RasterizedGlyph {
                    left: bounds.left + x0 as i32,
                    top: bounds.top + y0 as i32,
                    width: tw,
                    height: th,
                    advance_px,
                    pixels: Some(pixels),
                })
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::path::Path;

        fn load_font_file(path: &str, index: u32) -> Option<FontData> {
            let bytes = std::fs::read(path).ok()?;
            Some(FontData {
                font: Cow::Owned(bytes),
                index,
                tweak: Default::default(),
            })
        }

        #[test]
        fn dwrite_initializes_and_rasterizes_windows_and_subset_fonts_with_rgb_subpixels() {
            let sys = dwrite_system().expect("DirectWrite should initialize on Windows");
            assert!(
                (1.0..=2.6).contains(&sys.params.gamma),
                "unexpected gamma: {}",
                sys.params.gamma
            );
            assert!(
                (0.0..=2.0).contains(&sys.params.enhanced_contrast),
                "unexpected contrast: {}",
                sys.params.enhanced_contrast
            );

            // 1. Windows Segoe UI (Latin slanted/curved strokes must show true RGB subpixel offsets)
            if let Some(segoe) = load_font_file(r"C:\Windows\Fonts\segoeui.ttf", 0) {
                let face = DWriteFace::from_font_data(&segoe).expect("Segoe UI DWriteFace");
                for &sz in &[11.0_f32, 12.0, 13.0, 14.0] {
                    let gid = face.glyph_index_for_char('A').expect("glyph index for A");
                    assert_ne!(gid, 0);
                    let rast = face
                        .rasterize_glyph(gid, sz, 1.0, true)
                        .expect("rasterize A");
                    assert!(rast.width > 0 && rast.height > 0);
                    assert!(rast.advance_px > 0.0);
                    match rast.pixels.expect("non-empty pixels") {
                        GlyphPixels::SubpixelRgb(rgb) => {
                            let has_subpixel =
                                rgb.iter().any(|&[r, g, b]| r != g || g != b);
                            assert!(
                                has_subpixel,
                                "Segoe UI 'A' at {sz}px @ 100% DPI should have RGB subpixel coverage"
                            );
                        }
                        GlyphPixels::Grayscale(_) => panic!("expected SubpixelRgb"),
                    }
                }
            }

            // 2. Windows Microsoft YaHei TTC (`msyh.ttc`, index 0)
            if let Some(msyh) = load_font_file(r"C:\Windows\Fonts\msyh.ttc", 0) {
                let face = DWriteFace::from_font_data(&msyh).expect("msyh.ttc DWriteFace");
                for &sz in &[11.0_f32, 12.0, 13.0, 14.0] {
                    let gid = face.glyph_index_for_char('部').expect("glyph index for 部");
                    assert_ne!(gid, 0);
                    let rast = face
                        .rasterize_glyph(gid, sz, 1.0, true)
                        .expect("rasterize 部");
                    assert!(rast.width > 0 && rast.height > 0);
                    assert!(rast.advance_px > 0.0);
                    match rast.pixels.expect("non-empty pixels") {
                        GlyphPixels::SubpixelRgb(rgb) => {
                            assert!(
                                rgb.iter().any(|&[r, g, b]| r > 0 || g > 0 || b > 0),
                                "msyh '部' at {sz}px should have ink"
                            );
                            assert!(
                                rgb.iter().any(|&[r, g, b]| r != g || g != b),
                                "msyh '部' at {sz}px should have RGB subpixel variation"
                            );
                        }
                        GlyphPixels::Grayscale(_) => panic!("expected SubpixelRgb"),
                    }
                }
            }

            // 3. Bundled NotoSansSC-Medium.subset.ttf in repo assets
            let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
            let subset_path = manifest_dir
                .join("..")
                .join("..")
                .join("assets")
                .join("fonts")
                .join("subset")
                .join("NotoSansSC-Medium.subset.ttf");
            if subset_path.exists() {
                let bytes = std::fs::read(&subset_path).expect("read NotoSansSC-Medium subset");
                let fd = FontData::from_owned(bytes);
                let face =
                    DWriteFace::from_font_data(&fd).expect("NotoSansSC-Medium DWriteFace");
                for &sz in &[11.0_f32, 12.0, 13.0, 14.0] {
                    let gid = face.glyph_index_for_char('营').expect("glyph index for 营");
                    assert_ne!(gid, 0);
                    let rast = face
                        .rasterize_glyph(gid, sz, 1.0, true)
                        .expect("rasterize 营");
                    assert!(rast.width > 0 && rast.height > 0);
                    match rast.pixels.expect("non-empty pixels") {
                        GlyphPixels::SubpixelRgb(rgb) => {
                            assert!(
                                rgb.iter().any(|&[r, g, b]| r != g || g != b),
                                "NotoSansSC-Medium '营' at {sz}px should have RGB subpixel coverage"
                            );
                        }
                        GlyphPixels::Grayscale(_) => panic!("expected SubpixelRgb"),
                    }
                }
            }
        }

        #[test]
        fn dwrite_simsun_12px_produces_crisp_hinted_glyphs() {
            let Some(simsun) = load_font_file(r"C:\Windows\Fonts\simsun.ttc", 0) else {
                return;
            };
            let face = DWriteFace::from_font_data(&simsun).expect("simsun.ttc DWriteFace");
            let gid = face.glyph_index_for_char('表').expect("glyph index for 表");
            assert_ne!(gid, 0);
            let rast = face
                .rasterize_glyph(gid, 12.0, 1.0, true)
                .expect("SimSun 12px should rasterize cleanly");
            assert!(rast.width >= 8 && rast.height >= 8, "w={} h={}", rast.width, rast.height);
            assert!(rast.advance_px >= 10.0, "advance={}", rast.advance_px);
            match rast.pixels.expect("SimSun ink") {
                GlyphPixels::SubpixelRgb(rgb) => {
                    assert!(rgb.iter().any(|&[r, g, b]| r > 128 || g > 128 || b > 128));
                }
                GlyphPixels::Grayscale(_) => panic!("expected SubpixelRgb"),
            }
        }

        #[test]
        fn dwrite_hidpi_150_emits_pure_neutral_grayscale_without_color_fringing() {
            let Some(msyh) = load_font_file(r"C:\Windows\Fonts\msyh.ttc", 0) else {
                return;
            };
            let face = DWriteFace::from_font_data(&msyh).expect("msyh.ttc DWriteFace");
            for ch in ['数', '据', '验', '证', '1', '8', '.'] {
                let gid = face.glyph_index_for_char(ch).expect("glyph index");
                assert_ne!(gid, 0);
                let rast = face
                    .rasterize_glyph(gid, 13.0, 1.5, true)
                    .expect("rasterize at 150% DPI");
                match rast.pixels.expect("non-empty pixels") {
                    GlyphPixels::SubpixelRgb(rgb) => {
                        assert!(rgb.iter().any(|&[r, g, b]| r > 64 || g > 64 || b > 64));
                        assert!(
                            rgb.iter().all(|&[r, g, b]| r == g && g == b),
                            "150% HiDPI '{ch}' must have zero RGB color fringing"
                        );
                    }
                    GlyphPixels::Grayscale(_) => panic!("expected SubpixelRgb"),
                }
            }
        }

        #[test]
        fn dwrite_100dpi_bounds_subpixel_chroma_while_preserving_refinement() {
            let Some(segoe) = load_font_file(r"C:\Windows\Fonts\segoeui.ttf", 0) else {
                return;
            };
            let face = DWriteFace::from_font_data(&segoe).expect("Segoe UI DWriteFace");
            let mut max_spread = 0u8;
            let mut saw_subpixel = false;
            for ch in ['0', '1', '2', '3', '4', '5', '6', '7', '8', '9', '.', 'A', '¥'] {
                let Some(gid) = face.glyph_index_for_char(ch) else {
                    continue;
                };
                if gid == 0 {
                    continue;
                }
                let rast = face
                    .rasterize_glyph(gid, 12.0, 1.0, true)
                    .expect("rasterize at 100% DPI");
                if let Some(GlyphPixels::SubpixelRgb(rgb)) = rast.pixels {
                    for &[r, g, b] in &rgb {
                        let hi = r.max(g).max(b);
                        let lo = r.min(g).min(b);
                        let spread = hi - lo;
                        max_spread = max_spread.max(spread);
                        if spread > 0 {
                            saw_subpixel = true;
                        }
                    }
                }
            }
            assert!(saw_subpixel, "100% DPI should retain subtle RGB subpixel precision");
            assert!(
                max_spread <= 105,
                "100% DPI subpixel chroma spread too high (fringing risk): max_spread={max_spread}"
            );
        }

        #[test]
        fn dwrite_invalid_or_empty_font_bytes_fall_back_gracefully_without_panic() {
            let empty = FontData::from_owned(Vec::new());
            assert!(DWriteFace::from_font_data(&empty).is_none());

            let garbage = FontData::from_owned(b"not a valid font file at all 1234567890".to_vec());
            assert!(DWriteFace::from_font_data(&garbage).is_none());

            // Out-of-range TTC index should also fail cleanly without panic.
            if let Some(mut msyh) = load_font_file(r"C:\Windows\Fonts\msyh.ttc", 0) {
                msyh.index = 999;
                assert!(DWriteFace::from_font_data(&msyh).is_none());
            }
        }

        #[test]
        fn dwrite_em_size_baseline_and_glyph_ids_match_ab_glyph() {
            use ab_glyph::{Font as _, ScaleFont as _};

            let sys = dwrite_system().expect("DirectWrite system");
            eprintln!(
                "DWrite OS params: gamma={}, enhanced_contrast={}, clear_type_level={}, pixel_geometry={}",
                sys.params.gamma,
                sys.params.enhanced_contrast,
                sys.params.clear_type_level,
                sys.params.pixel_geometry,
            );

            let cases: &[(&str, u32, &[char])] = &[
                (r"C:\Windows\Fonts\segoeui.ttf", 0, &['A', 'g', 'H', '0']),
                (r"C:\Windows\Fonts\msyh.ttc", 0, &['部', '营', '表', '开']),
            ];

            for &(path, idx, chars) in cases {
                let Some(fd) = load_font_file(path, idx) else {
                    continue;
                };
                let dw = DWriteFace::from_font_data(&fd).expect("DWriteFace");
                let ab = ab_glyph::FontVec::try_from_vec_and_index(fd.font.into_owned(), idx)
                    .expect("ab_glyph");
                let upem = ab.units_per_em().unwrap();
                let font_scaling = ab.height_unscaled() / upem;

                for &ppp in &[1.0_f32, 1.5] {
                    for &pt in &[11.0_f32, 12.0, 13.0, 14.0] {
                        let ab_px_scale = (pt * ppp * font_scaling).round();
                        for &ch in chars {
                            let ab_gid = ab.glyph_id(ch);
                            let dw_gid = dw.glyph_index_for_char(ch).unwrap();
                            assert_eq!(
                                ab_gid.0, dw_gid,
                                "glyph id mismatch for {ch:?} in {path}"
                            );

                            let rast = dw
                                .rasterize_glyph(dw_gid, pt, ppp, true)
                                .expect("dw rasterize");
                            let ab_glyph = ab_gid.with_scale_and_position(
                                ab_px_scale,
                                ab_glyph::Point { x: 0.0, y: 0.0 },
                            );
                            let bb = ab.outline_glyph(ab_glyph).unwrap().px_bounds();
                            let ab_top = bb.min.y.round() as i32;
                            let ab_h = bb.height().round() as i32;
                            assert!(
                                (rast.top - ab_top).abs() <= 2,
                                "baseline drift for {ch:?} @ {pt}pt ppp={ppp} in {path}: dw_top={} vs ab_top={ab_top}",
                                rast.top
                            );
                            assert!(
                                (rast.height as i32 - ab_h).abs() <= 2,
                                "height drift for {ch:?} @ {pt}pt ppp={ppp} in {path}: dw_h={} vs ab_h={ab_h}",
                                rast.height
                            );
                            let ab_adv = ab.as_scaled(ab_px_scale).h_advance(ab_gid);
                            assert!(
                                (rast.advance_px - ab_adv).abs() <= 1.5,
                                "advance drift for {ch:?} @ {pt}pt ppp={ppp} in {path}: dw_adv={} vs ab_adv={ab_adv}",
                                rast.advance_px
                            );
                        }
                    }
                }
            }
        }

        #[test]
        fn dwrite_trims_empty_filter_apron_for_non_overhanging_glyphs() {
            // Verify non-overhanging glyphs do not get a negative left offset from
            // DirectWrite's 1px horizontal ClearType filter padding in either mode.
            for (path, chars) in [
                (r"C:\Windows\Fonts\segoeui.ttf", &['A', 'H', '0'][..]),
                (r"C:\Windows\Fonts\msyh.ttc", &['查', '部', '营'][..]),
            ] {
                let Some(fd) = load_font_file(path, 0) else {
                    continue;
                };
                let face = DWriteFace::from_font_data(&fd).expect("DWriteFace");
                for &subpixel in &[false, true] {
                    for &ch in chars {
                        let gid = face.glyph_index_for_char(ch).unwrap();
                        let rast = face
                            .rasterize_glyph(gid, 12.0, 1.0, subpixel)
                            .expect("rasterize");
                        assert!(
                            rast.left >= 0,
                            "non-overhanging glyph {ch:?} in {path} (subpixel={subpixel}) had negative left={}",
                            rast.left
                        );
                        assert!(rast.width > 0 && rast.height > 0);
                    }
                }
            }
        }
    }
}
