use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RemoteId(pub i32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProviderKind {
    Dropbox,
    GDrive,
    /// A folder on this device (rclone's `alias` onto it), e.g. a USB disk or a network share mounted by the system.
    Local,
    Nextcloud,
    OneDrive,
    ProtonDrive,
}

impl ProviderKind {
    /// Map a backend type string to our domain enum. Accepts both
    /// rclone backend names ("drive", "dropbox", …) and the native
    /// backend sentinel ("native-proton"). Unrecognised strings return
    /// `None` — treated as "no provider-specific quirks".
    pub fn from_rclone_type(t: &str) -> Option<Self> {
        match t {
            "dropbox" => Some(Self::Dropbox),
            "drive" => Some(Self::GDrive),
            "alias" => Some(Self::Local),
            "onedrive" => Some(Self::OneDrive),
            // Only the native client reports Proton now; rclone's
            // `protondrive` backend is no longer compiled into the
            // Go archive (see src/go/wrapper.go).
            "native-proton" => Some(Self::ProtonDrive),
            // Nextcloud is the WebDAV remote Syncative sets up.
            "webdav" => Some(Self::Nextcloud),
            _ => None,
        }
    }

    /// Interval a fresh remote of this kind should be created with.
    pub fn default_interval(self) -> Interval {
        Interval::FifteenSeconds
    }

    /// Intervals shorter than this are flagged in the UI with a ⚠.
    /// Above it, the backend should cope without the warning. `None`
    /// means no warnings for this provider.
    pub fn short_interval_threshold(self) -> Option<Interval> {
        None
    }

    /// Text shown on the tooltip next to the picker when the provider
    /// has short-interval warnings.
    pub fn short_interval_warning(self) -> Option<&'static str> {
        None
    }

    /// True when this provider uses rclone as its transport and therefore
    /// can benefit from the stderr-tap rate-limit detection in the rclone
    /// translator. ProtonDrive reports rate limits via its own error paths,
    /// so it doesn't need the tap.
    pub fn uses_rclone_transport(self) -> bool {
        !matches!(self, Self::ProtonDrive)
    }

    /// Offered only with the beta features on; such remotes carry the beta mark after their name. The one list of beta providers: taking one out here drops its mark from the picker and from every remote of it, as names are stored without the mark.
    pub fn is_beta(self) -> bool {
        matches!(
            self,
            Self::Dropbox | Self::Nextcloud | Self::OneDrive | Self::Local
        )
    }
}

#[derive(Clone, Debug)]
pub struct Remote {
    pub id: RemoteId,
    pub name: String,
    pub policy: SyncPolicy,
    /// Filled in after load by the app layer via `BackendClient::remote_type`.
    /// Drives provider-specific UI hints (interval warnings, defaults).
    pub provider_kind: Option<ProviderKind>,
    /// Which adapter owns this remote at runtime. New rows default
    /// to `Backend::Rclone`; ProtonDrive remotes added via the
    /// native auth flow are stamped `Backend::NativeProton`.
    pub backend: Backend,
    /// For native-backend remotes, the filesystem path to the
    /// persisted session blob that `syncative-go` reads with
    /// `ProtonDrive_ResumeSession`. `None` for rclone remotes.
    pub session_path: Option<String>,
}

impl Remote {
    /// The name as the window shows it: with the beta mark (🧪) for a provider that's a beta feature.
    pub fn title(&self) -> String {
        crate::services::beta::marked(
            &self.name,
            self.provider_kind.is_some_and(ProviderKind::is_beta),
        )
    }
}

/// The space a provider reports for a remote. `total` is the quota —
/// `None` when the backend knows only what's used (or reports the
/// quota as unlimited).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteUsage {
    pub used: u64,
    pub total: Option<u64>,
}

/// Which adapter drives a given remote. Kept as a plain enum rather
/// than a trait object so it can be persisted to the DB as a string
/// and pattern-matched on in the sync scheduler's routing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Backend {
    /// librclone's RPC surface — every backend rclone supports.
    Rclone,
    /// Native ProtonDrive client in `infrastructure::proton`.
    NativeProton,
}

impl Backend {
    /// Parse the DB-stored string form. Unknown strings fall back
    /// to `Rclone` — old rows (or rows from a future schema we
    /// haven't learned about yet) keep working.
    pub fn from_db_str(s: &str) -> Self {
        match s {
            "native-proton" => Self::NativeProton,
            _ => Self::Rclone,
        }
    }

    /// The string form stored in the DB.
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Rclone => "rclone",
            Self::NativeProton => "native-proton",
        }
    }
}

/// Per-remote sync cadence. Fixed to a handful of discrete choices —
/// any other value coming out of the DB is rounded to the nearest one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncPolicy {
    pub interval: Interval,
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interval {
    FiveSeconds,
    FifteenSeconds,
    ThirtySeconds,
    OneMinute,
    FiveMinutes,
    FifteenMinutes,
    ThirtyMinutes,
    OneHour,
}

impl Interval {
    pub const ALL: [Interval; 8] = [
        Interval::FiveSeconds,
        Interval::FifteenSeconds,
        Interval::ThirtySeconds,
        Interval::OneMinute,
        Interval::FiveMinutes,
        Interval::FifteenMinutes,
        Interval::ThirtyMinutes,
        Interval::OneHour,
    ];

    pub fn duration(self) -> Duration {
        Duration::from_secs(self.seconds())
    }

    pub fn seconds(self) -> u64 {
        match self {
            Interval::FiveSeconds => 5,
            Interval::FifteenSeconds => 15,
            Interval::ThirtySeconds => 30,
            Interval::OneMinute => 60,
            Interval::FiveMinutes => 300,
            Interval::FifteenMinutes => 900,
            Interval::ThirtyMinutes => 1_800,
            Interval::OneHour => 3_600,
        }
    }

    /// Map an arbitrary seconds value to the closest supported choice.
    pub fn from_seconds(secs: u64) -> Self {
        Self::ALL
            .into_iter()
            .min_by_key(|i| i.seconds().abs_diff(secs))
            .unwrap_or(Interval::FifteenSeconds)
    }
}

impl Default for SyncPolicy {
    fn default() -> Self {
        Self {
            interval: Interval::FifteenSeconds,
            enabled: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote_with(kind: Option<ProviderKind>, picked: Interval) -> Remote {
        Remote {
            id: RemoteId(1),
            name: "Remote".to_owned(),
            policy: SyncPolicy {
                interval: picked,
                enabled: true,
            },
            provider_kind: kind,
            backend: Backend::Rclone,
            session_path: None,
        }
    }

    /// The mark comes from the provider kind at display time, never from the stored name, so it disappears with the kind's beta status.
    #[test]
    fn the_beta_mark_follows_the_provider_kind() {
        let nextcloud = remote_with(Some(ProviderKind::Nextcloud), Interval::FifteenSeconds);
        assert_eq!(nextcloud.name, "Remote");
        assert_eq!(
            nextcloud.title(),
            if ProviderKind::Nextcloud.is_beta() {
                "Remote 🧪"
            } else {
                "Remote"
            }
        );
        for kind in [ProviderKind::ProtonDrive, ProviderKind::GDrive] {
            assert!(!kind.is_beta());
            assert_eq!(
                remote_with(Some(kind), Interval::FifteenSeconds).title(),
                "Remote"
            );
        }
        assert_eq!(
            remote_with(None, Interval::FifteenSeconds).title(),
            "Remote"
        );
    }

    #[test]
    fn all_providers_default_to_fifteen_seconds() {
        assert_eq!(
            ProviderKind::ProtonDrive.default_interval(),
            Interval::FifteenSeconds,
        );
        assert_eq!(
            ProviderKind::GDrive.default_interval(),
            Interval::FifteenSeconds,
        );
        assert_eq!(
            ProviderKind::Nextcloud.default_interval(),
            Interval::FifteenSeconds,
        );
    }

    #[test]
    fn short_interval_threshold_is_none_for_all_providers() {
        assert_eq!(ProviderKind::ProtonDrive.short_interval_threshold(), None);
        assert_eq!(ProviderKind::GDrive.short_interval_threshold(), None);
        assert_eq!(ProviderKind::Dropbox.short_interval_threshold(), None);
    }

    #[test]
    fn user_picking_still_survives_through_the_policy() {
        // No clamping anywhere: whatever the user picks is what we use.
        let r = remote_with(Some(ProviderKind::ProtonDrive), Interval::FiveSeconds);
        assert_eq!(r.policy.interval.duration(), Duration::from_secs(5));
    }

    #[test]
    fn rclone_type_mapping_covers_known_backends() {
        assert_eq!(
            ProviderKind::from_rclone_type("native-proton"),
            Some(ProviderKind::ProtonDrive)
        );
        assert_eq!(
            ProviderKind::from_rclone_type("drive"),
            Some(ProviderKind::GDrive)
        );
        assert_eq!(
            ProviderKind::from_rclone_type("dropbox"),
            Some(ProviderKind::Dropbox)
        );
        assert_eq!(
            ProviderKind::from_rclone_type("onedrive"),
            Some(ProviderKind::OneDrive)
        );
        assert_eq!(
            ProviderKind::from_rclone_type("webdav"),
            Some(ProviderKind::Nextcloud)
        );
        assert_eq!(ProviderKind::from_rclone_type("bogus"), None);
    }

    #[test]
    fn proton_drive_skips_stderr_probe() {
        // The native client surfaces rate-limits via its own error paths,
        // so the rclone-stderr tap must never flag a ProtonDrive pass as
        // degraded.
        assert!(!ProviderKind::ProtonDrive.uses_rclone_transport());
        assert!(ProviderKind::GDrive.uses_rclone_transport());
    }
}
