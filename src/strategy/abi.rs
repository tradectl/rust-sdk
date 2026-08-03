//! Compile-time ABI layout fingerprint for the strategy-plugin boundary.
//!
//! ## Why this exists
//!
//! The plugin ABI passes `#[repr(Rust)]` types (`StrategyContext`, `Action`,
//! `FillEvent`, `ExitOrder`, `PositionInfo`, `EntryOrder`, …) across the
//! dylib boundary between the CLI and a strategy `.so`/`.dylib`. Rust guarantees
//! **nothing** about `#[repr(Rust)]` field layout across compiler versions, so
//! that boundary is only sound when the CLI and the strategy were built by the
//! *same rustc* against the *same SDK source*. [`STRATEGY_ABI_VERSION`] is a
//! hand-bumped integer — it does not reflect layout — so a plugin built on a
//! different rustc passes the version check and then reads every struct at the
//! wrong offsets, corrupting memory (2026-07-14 bnum/bncm prod incident: a
//! `shot.so` built on rustc 1.95.0 loaded by a CLI built on 1.97.0 aborted on a
//! garbage-sized allocation).
//!
//! ## How it works
//!
//! [`ABI_LAYOUT_FINGERPRINT`] is a `const` folded from `size_of` + `align_of` +
//! per-field `offset_of!` of every type that crosses the boundary. It lives in
//! this crate, so it is compiled into **both** sides — but each side computes it
//! with *its own* compiler's layout. If the two compilers lay the structs out
//! identically the fingerprints match; if they differ in any way that matters
//! they don't. The plugin exports its fingerprint in the `#[repr(C)]`
//! [`StrategyPlugin`] header (whose layout *is* compiler-stable), and the loader
//! calls [`check_plugin_abi`] to refuse a mismatched plugin at load — turning
//! silent mid-trade memory corruption into a clean refuse-to-start.
//!
//! Per-field offsets (not just size/align) are required because a field
//! *reorder* that preserves total size — e.g. two `String` fields swapped —
//! would otherwise slip through.

use std::mem::{align_of, offset_of, size_of};

use crate::types::{
    DepthLevel, MaSeries, OrderBookDepth, ParamDef, Params, Side, TickerEvent, TradeEvent,
    VolumeProfile,
};

use super::batch::BATCH_TRAIT_REVISION;
use super::{
    Action, BatchConfig, BatchDiagnostics, BatchExchange, BatchResult, EntryOrder, ExitOrder,
    ExitType, FillEvent, FillResponse, MonitorSnapshot, OrderKind, PositionInfo, PriceLine,
    StrategyContext, StrategyPlugin, STRATEGY_ABI_VERSION,
};

/// tradectl-sdk version this side was built with (diagnostics only).
pub const SDK_VERSION: &str = env!("CARGO_PKG_VERSION");

/// rustc version this side of the boundary was built with (diagnostics only).
/// Set by `build.rs`; the differing values on the two sides are what a layout
/// mismatch is usually attributable to.
pub const SDK_RUSTC_VERSION: &str = env!("TRADECTL_RUSTC_VERSION");

// FNV-1a (64-bit) constants — a tiny order-sensitive const hash.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Fold one `u64` value into the running hash (FNV-1a over its 8 LE bytes).
const fn mix(mut h: u64, v: u64) -> u64 {
    let bytes = v.to_le_bytes();
    let mut i = 0;
    while i < 8 {
        h ^= bytes[i] as u64;
        h = h.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    h
}

/// A compiler-derived fingerprint of the plugin-boundary type layout.
///
/// Equal on both sides iff their compilers lay every boundary type out
/// identically. See the module docs.
pub const ABI_LAYOUT_FINGERPRINT: u64 = compute_fingerprint();

const fn compute_fingerprint() -> u64 {
    let mut h = FNV_OFFSET;

    // -- StrategyContext (the fattest boundary struct; references + lifetime) --
    h = mix(h, size_of::<StrategyContext<'static>>() as u64);
    h = mix(h, align_of::<StrategyContext<'static>>() as u64);
    h = mix(h, offset_of!(StrategyContext<'static>, timestamp_ms) as u64);
    h = mix(h, offset_of!(StrategyContext<'static>, book) as u64);
    h = mix(h, offset_of!(StrategyContext<'static>, positions) as u64);
    h = mix(h, offset_of!(StrategyContext<'static>, balance) as u64);
    h = mix(h, offset_of!(StrategyContext<'static>, unrealized_pnl) as u64);
    h = mix(h, offset_of!(StrategyContext<'static>, realized_pnl) as u64);
    h = mix(h, offset_of!(StrategyContext<'static>, trade_count) as u64);
    h = mix(h, offset_of!(StrategyContext<'static>, direction) as u64);
    h = mix(h, offset_of!(StrategyContext<'static>, max_orders_reached) as u64);
    h = mix(h, offset_of!(StrategyContext<'static>, depth) as u64);
    h = mix(h, offset_of!(StrategyContext<'static>, volume) as u64);
    h = mix(h, offset_of!(StrategyContext<'static>, can_enter) as u64);
    h = mix(h, offset_of!(StrategyContext<'static>, entry_orders) as u64);
    h = mix(h, offset_of!(StrategyContext<'static>, ma) as u64);

    // -- PositionInfo --
    h = mix(h, size_of::<PositionInfo>() as u64);
    h = mix(h, align_of::<PositionInfo>() as u64);
    h = mix(h, offset_of!(PositionInfo, side) as u64);
    h = mix(h, offset_of!(PositionInfo, avg_entry) as u64);
    h = mix(h, offset_of!(PositionInfo, quantity) as u64);
    h = mix(h, offset_of!(PositionInfo, total_entered) as u64);
    h = mix(h, offset_of!(PositionInfo, entry_count) as u64);
    h = mix(h, offset_of!(PositionInfo, last_entry_price) as u64);

    // -- EntryOrder --
    h = mix(h, size_of::<EntryOrder>() as u64);
    h = mix(h, align_of::<EntryOrder>() as u64);
    h = mix(h, offset_of!(EntryOrder, slot) as u64);
    h = mix(h, offset_of!(EntryOrder, side) as u64);
    h = mix(h, offset_of!(EntryOrder, price) as u64);
    h = mix(h, offset_of!(EntryOrder, size) as u64);
    h = mix(h, offset_of!(EntryOrder, filled) as u64);

    // -- ExitOrder --
    h = mix(h, size_of::<ExitOrder>() as u64);
    h = mix(h, align_of::<ExitOrder>() as u64);
    h = mix(h, offset_of!(ExitOrder, id) as u64);
    h = mix(h, offset_of!(ExitOrder, price) as u64);
    h = mix(h, offset_of!(ExitOrder, size) as u64);
    h = mix(h, offset_of!(ExitOrder, kind) as u64);
    h = mix(h, offset_of!(ExitOrder, delay_ms) as u64);

    // -- FillEvent --
    h = mix(h, size_of::<FillEvent>() as u64);
    h = mix(h, align_of::<FillEvent>() as u64);
    h = mix(h, offset_of!(FillEvent, order_id) as u64);
    h = mix(h, offset_of!(FillEvent, symbol) as u64);
    h = mix(h, offset_of!(FillEvent, price) as u64);
    h = mix(h, offset_of!(FillEvent, quantity) as u64);
    h = mix(h, offset_of!(FillEvent, is_entry) as u64);
    h = mix(h, offset_of!(FillEvent, is_partial) as u64);
    h = mix(h, offset_of!(FillEvent, exit_id) as u64);
    h = mix(h, offset_of!(FillEvent, position_closed) as u64);

    // -- FillResponse --
    h = mix(h, size_of::<FillResponse>() as u64);
    h = mix(h, align_of::<FillResponse>() as u64);
    h = mix(h, offset_of!(FillResponse, actions) as u64);
    h = mix(h, offset_of!(FillResponse, notify) as u64);

    // -- PriceLine --
    h = mix(h, size_of::<PriceLine>() as u64);
    h = mix(h, align_of::<PriceLine>() as u64);
    h = mix(h, offset_of!(PriceLine, label) as u64);
    h = mix(h, offset_of!(PriceLine, price) as u64);
    h = mix(h, offset_of!(PriceLine, color) as u64);
    h = mix(h, offset_of!(PriceLine, style) as u64);
    h = mix(h, offset_of!(PriceLine, line_width) as u64);
    h = mix(h, offset_of!(PriceLine, axis_label) as u64);
    h = mix(h, offset_of!(PriceLine, param_name) as u64);
    h = mix(h, offset_of!(PriceLine, param_value) as u64);

    // -- MonitorSnapshot --
    h = mix(h, size_of::<MonitorSnapshot>() as u64);
    h = mix(h, align_of::<MonitorSnapshot>() as u64);
    h = mix(h, offset_of!(MonitorSnapshot, price_lines) as u64);
    h = mix(h, offset_of!(MonitorSnapshot, state) as u64);

    // -- ParamDef --
    h = mix(h, size_of::<ParamDef>() as u64);
    h = mix(h, align_of::<ParamDef>() as u64);
    h = mix(h, offset_of!(ParamDef, key) as u64);
    h = mix(h, offset_of!(ParamDef, description) as u64);
    h = mix(h, offset_of!(ParamDef, default) as u64);
    h = mix(h, offset_of!(ParamDef, min) as u64);
    h = mix(h, offset_of!(ParamDef, max) as u64);
    h = mix(h, offset_of!(ParamDef, step) as u64);

    // -- OrderBookDepth + DepthLevel --
    h = mix(h, size_of::<OrderBookDepth>() as u64);
    h = mix(h, align_of::<OrderBookDepth>() as u64);
    h = mix(h, offset_of!(OrderBookDepth, bids) as u64);
    h = mix(h, offset_of!(OrderBookDepth, asks) as u64);
    h = mix(h, offset_of!(OrderBookDepth, timestamp_ms) as u64);
    h = mix(h, size_of::<DepthLevel>() as u64);
    h = mix(h, align_of::<DepthLevel>() as u64);

    // -- VolumeProfile --
    h = mix(h, size_of::<VolumeProfile>() as u64);
    h = mix(h, align_of::<VolumeProfile>() as u64);
    h = mix(h, offset_of!(VolumeProfile, ratio) as u64);
    h = mix(h, offset_of!(VolumeProfile, baseline_per_min) as u64);
    h = mix(h, offset_of!(VolumeProfile, current_per_min) as u64);
    h = mix(h, offset_of!(VolumeProfile, buy_ratio) as u64);
    h = mix(h, offset_of!(VolumeProfile, baseline_ready) as u64);

    // -- Batch (SoA) boundary --
    //
    // `batch_factory` hands the driver a `Box<dyn BatchStrategy>` whose vtable
    // and argument structs cross the dylib boundary exactly like the scalar
    // ones do. Until this block existed nothing in the loader looked at them at
    // all (coverage FINDINGS F9): a plugin built against a different
    // `BatchConfig` was not refused, it was *called*, so the mismatch was UB
    // rather than a load error. Struct layout folds in automatically below;
    // the trait's method set has no const-observable shape, so a change to it
    // rides on [`BATCH_TRAIT_REVISION`], which is hand-bumped.
    h = mix(h, BATCH_TRAIT_REVISION as u64);
    h = mix(h, size_of::<BatchConfig>() as u64);
    h = mix(h, align_of::<BatchConfig>() as u64);
    h = mix(h, offset_of!(BatchConfig, initial_balance) as u64);
    h = mix(h, offset_of!(BatchConfig, market_type) as u64);
    h = mix(h, offset_of!(BatchConfig, ma_max_period) as u64);
    h = mix(h, offset_of!(BatchConfig, ma_interval_ms) as u64);
    h = mix(h, offset_of!(BatchConfig, ma_from_klines) as u64);
    h = mix(h, offset_of!(BatchConfig, ma_warmup_bars) as u64);
    h = mix(h, size_of::<BatchResult>() as u64);
    h = mix(h, align_of::<BatchResult>() as u64);
    h = mix(h, offset_of!(BatchResult, total_pnl) as u64);
    h = mix(h, offset_of!(BatchResult, calmar_ratio) as u64);
    h = mix(h, size_of::<BatchDiagnostics>() as u64);
    h = mix(h, align_of::<BatchDiagnostics>() as u64);
    h = mix(h, size_of::<BatchExchange>() as u64);
    h = mix(h, align_of::<BatchExchange>() as u64);
    h = mix(h, offset_of!(BatchExchange, n) as u64);
    h = mix(h, offset_of!(BatchExchange, entry_price) as u64);
    h = mix(h, offset_of!(BatchExchange, pos_active) as u64);
    h = mix(h, offset_of!(BatchExchange, balance) as u64);
    h = mix(h, offset_of!(BatchExchange, ma) as u64);
    h = mix(h, size_of::<MaSeries>() as u64);
    h = mix(h, align_of::<MaSeries>() as u64);

    // -- Enums / opaque types: size + align only (offset_of! doesn't apply to
    //    enum variants, and Params wraps a private HashMap). A layout change to
    //    any of these still shifts its size or alignment. --
    h = mix(h, size_of::<Action>() as u64);
    h = mix(h, align_of::<Action>() as u64);
    h = mix(h, size_of::<Side>() as u64);
    h = mix(h, align_of::<Side>() as u64);
    h = mix(h, size_of::<OrderKind>() as u64);
    h = mix(h, size_of::<ExitType>() as u64);
    h = mix(h, size_of::<TickerEvent>() as u64);
    h = mix(h, align_of::<TickerEvent>() as u64);
    h = mix(h, size_of::<TradeEvent>() as u64);
    h = mix(h, align_of::<TradeEvent>() as u64);
    h = mix(h, size_of::<Params>() as u64);
    h = mix(h, align_of::<Params>() as u64);

    h
}

/// Read a `(ptr, len)` string field from a plugin header. Safe to call only
/// after the ABI version has been confirmed to match (so the fields were
/// actually written by the plugin). Never panics.
///
/// # Safety
/// `ptr`/`len` must describe a valid UTF-8-ish byte range owned by the loaded
/// dylib (they are `'static` string constants on the plugin side).
unsafe fn read_plugin_str(ptr: *const u8, len: usize) -> String {
    if ptr.is_null() || len == 0 {
        return "unknown".to_string();
    }
    let slice = std::slice::from_raw_parts(ptr, len);
    String::from_utf8_lossy(slice).into_owned()
}

/// Verify a freshly-loaded plugin against this host build.
///
/// `Ok(())` — the plugin's ABI version and layout fingerprint match this CLI; it
/// is safe to call `factory()`.
///
/// `Err(msg)` — refuse to load; `msg` is a user-facing, actionable message.
///
/// The version check runs first and reads only `abi_version` (offset 0 of the
/// `#[repr(C)]` header, correct for any plugin). It rejects any plugin whose
/// version differs — including old plugins built before this header field
/// existed — *before* the fingerprint or diagnostic-string fields are read, so
/// no uninitialized tail is ever touched on the migration boundary.
///
/// `key` is the strategy key used in the `tradectl install <key>` hint; derive
/// it from the dylib path, not from the plugin's own (untrusted) memory.
pub fn check_plugin_abi(plugin: &StrategyPlugin, key: &str) -> Result<(), String> {
    if plugin.abi_version != STRATEGY_ABI_VERSION {
        return Err(format!(
            "strategy `{key}` was built for plugin ABI v{}, but this CLI expects v{}.\n  \
             Reinstall it to get a matching build:  tradectl install {key}\n  \
             (or rebuild it against this CLI's tradectl-sdk).",
            plugin.abi_version, STRATEGY_ABI_VERSION,
        ));
    }

    // The version matched, so this is a v-current plugin that wrote every header
    // field — the diagnostic pointers are valid to read.
    let (plugin_rustc, plugin_sdk) = unsafe {
        (
            read_plugin_str(plugin.rustc_version, plugin.rustc_version_len),
            read_plugin_str(plugin.sdk_version, plugin.sdk_version_len),
        )
    };

    // (1) rustc-version gate — the primary, load-bearing check.
    //
    // Rust has NO stable ABI: two rustc versions are not guaranteed to
    // interoperate across a dylib boundary EVEN WHEN every type has identical
    // size/align/field-offset. The 2026-07-14 incident was exactly this — a
    // strategy built on 1.95.0 and a CLI on 1.97.0, byte-identical layouts, yet
    // the process aborted on a garbage-sized allocation when the CLI dropped a
    // String the plugin had constructed. A matched-toolchain 2x2 confirmed the
    // failure is purely the compiler-version mismatch, in both directions. So
    // the only sound invariant is "same rustc built both sides".
    if plugin_rustc != SDK_RUSTC_VERSION {
        return Err(format!(
            "strategy `{key}` was built with a different Rust compiler than this CLI and cannot be loaded safely.\n  \
             built with:  {plugin_rustc}  (tradectl-sdk {plugin_sdk})\n  \
             this CLI:    {host_rustc}  (tradectl-sdk {host_sdk})\n  \
             Rust has no stable ABI across compiler versions, so mismatched builds corrupt \
             memory at runtime even when the types look identical.\n  \
             Reinstall it to get a matching build:  tradectl install {key}\n  \
             (or rebuild it with the same rustc — see rust-toolchain.toml).",
            host_rustc = SDK_RUSTC_VERSION,
            host_sdk = SDK_VERSION,
        ));
    }

    // (2) Source-layout fingerprint gate — complementary. Catches the *other*
    // way a plugin can be incompatible: same rustc, but built against a
    // different SDK *source* (a boundary struct field reordered/added) without
    // bumping the ABI version. Layout then differs and this catches it. It does
    // NOT catch compiler-version drift (that leaves layout identical — hence the
    // rustc gate above is primary, not this).
    if plugin.abi_fingerprint != ABI_LAYOUT_FINGERPRINT {
        return Err(format!(
            "strategy `{key}` was built against an incompatible tradectl-sdk source and cannot be loaded safely.\n  \
             built with:  tradectl-sdk {plugin_sdk} ({plugin_rustc})\n  \
             this CLI:    tradectl-sdk {host_sdk} ({host_rustc})\n  \
             A plugin-boundary struct layout differs (fingerprint {:#018x} != {:#018x}); loading it \
             would corrupt memory.\n  \
             Reinstall it to get a matching build:  tradectl install {key}\n  \
             (or rebuild it against this CLI's tradectl-sdk).",
            plugin.abi_fingerprint,
            ABI_LAYOUT_FINGERPRINT,
            host_sdk = SDK_VERSION,
            host_rustc = SDK_RUSTC_VERSION,
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::{BatchFactory, Strategy};
    use crate::types::Params;

    struct Dummy;
    impl Strategy for Dummy {
        fn name(&self) -> &str {
            "dummy"
        }
    }

    fn dummy_factory(_: &Params) -> Box<dyn Strategy> {
        Box::new(Dummy)
    }

    fn host_plugin() -> StrategyPlugin {
        StrategyPlugin {
            abi_version: STRATEGY_ABI_VERSION,
            name: b"dummy".as_ptr(),
            name_len: 5,
            factory: dummy_factory,
            batch_factory: None::<BatchFactory>,
            abi_fingerprint: ABI_LAYOUT_FINGERPRINT,
            rustc_version: SDK_RUSTC_VERSION.as_ptr(),
            rustc_version_len: SDK_RUSTC_VERSION.len(),
            sdk_version: SDK_VERSION.as_ptr(),
            sdk_version_len: SDK_VERSION.len(),
        }
    }

    #[test]
    fn fingerprint_is_nonzero_and_deterministic() {
        assert_ne!(ABI_LAYOUT_FINGERPRINT, 0);
        assert_eq!(ABI_LAYOUT_FINGERPRINT, compute_fingerprint());
    }

    #[test]
    fn mix_is_order_sensitive() {
        // Guards the fingerprint against field *reorders* that preserve size:
        // folding the same offsets in a different order must change the hash.
        let a = mix(mix(FNV_OFFSET, 8), 16);
        let b = mix(mix(FNV_OFFSET, 16), 8);
        assert_ne!(a, b);
    }

    #[test]
    fn accepts_matching_plugin() {
        assert!(check_plugin_abi(&host_plugin(), "dummy").is_ok());
    }

    #[test]
    fn rejects_rustc_mismatch() {
        // The load-bearing gate: a plugin built by a different rustc is refused
        // even though its fingerprint is identical (layout is compiler-stable).
        // Guaranteed != host regardless of which rustc runs this test.
        const OTHER: &str = concat!("rustc-other-", env!("CARGO_PKG_VERSION"));
        assert_ne!(OTHER, SDK_RUSTC_VERSION);
        let mut p = host_plugin();
        p.rustc_version = OTHER.as_ptr();
        p.rustc_version_len = OTHER.len();
        // fingerprint intentionally left matching — proves rustc is what rejects.
        assert_eq!(p.abi_fingerprint, ABI_LAYOUT_FINGERPRINT);
        let err = check_plugin_abi(&p, "shot").unwrap_err();
        assert!(err.contains("different Rust compiler"), "got: {err}");
        assert!(err.contains("tradectl install shot"), "got: {err}");
    }

    #[test]
    fn rejects_layout_mismatch() {
        // Same rustc, different SDK source layout → the complementary gate fires.
        let mut p = host_plugin();
        p.abi_fingerprint ^= 0xDEAD_BEEF;
        let err = check_plugin_abi(&p, "shot").unwrap_err();
        assert!(err.contains("incompatible tradectl-sdk source"), "got: {err}");
        assert!(err.contains("tradectl install shot"), "got: {err}");
    }

    #[test]
    fn rejects_old_abi_version_before_reading_tail() {
        let mut p = host_plugin();
        p.abi_version = STRATEGY_ABI_VERSION - 1;
        // Simulate an old plugin that never wrote the fingerprint/version tail.
        p.abi_fingerprint = 0;
        p.rustc_version = std::ptr::null();
        p.rustc_version_len = 0;
        let err = check_plugin_abi(&p, "shot").unwrap_err();
        assert!(err.contains("plugin ABI v"), "got: {err}");
    }
}
