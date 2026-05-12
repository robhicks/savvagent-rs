//! ID newtypes and small structural types crossing plugin boundaries.

/// Stable opaque identifier for a registered plugin.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PluginId(
    /// The plugin's id string (`internal:`-prefixed for built-ins).
    pub String,
);

/// Stable opaque identifier for an LLM provider.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProviderId(
    /// The provider's id string (e.g. `"anthropic"`).
    pub String,
);

/// Opaque handle that identifies a live terminal screen instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScreenInstanceId(
    /// Numeric handle assigned by the TUI runtime.
    pub u32,
);

/// Axis-aligned rectangle within the terminal grid (columns × rows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    /// X coordinate (columns from the terminal left edge).
    pub x: u16,
    /// Y coordinate (rows from the terminal top edge).
    pub y: u16,
    /// Width in columns.
    pub width: u16,
    /// Height in rows.
    pub height: u16,
}

/// Wall-clock instant with sub-second precision, WIT-portable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timestamp {
    /// Seconds since the Unix epoch (may be negative for pre-epoch instants).
    pub secs: i64,
    /// Sub-second nanoseconds in the range `0..1_000_000_000`.
    pub nanos: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_string_newtypes() {
        let p = PluginId("internal:themes".to_string());
        let q = ProviderId("anthropic".to_string());
        assert_eq!(p.0, "internal:themes");
        assert_eq!(q.0, "anthropic");
    }

    #[test]
    fn region_fields_are_u16() {
        let r = Region { x: 0, y: 0, width: 80, height: 24 };
        let area: u32 = r.width as u32 * r.height as u32;
        assert_eq!(area, 1920);
    }

    #[test]
    fn timestamp_is_i64_secs_plus_u32_nanos() {
        let t = Timestamp { secs: 1_700_000_000, nanos: 500_000_000 };
        assert_eq!(t.secs, 1_700_000_000);
        assert_eq!(t.nanos, 500_000_000);
    }

    #[test]
    fn screen_instance_id_is_u32() {
        let s = ScreenInstanceId(42);
        assert_eq!(s.0, 42u32);
    }
}
