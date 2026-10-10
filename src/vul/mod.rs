//! Image vulnerability scans (k9s internal/vul). k9s links grype v0.104.2 as a
//! library; k9rs runs the `grype` CLI of that version (`$K9RS_GRYPE` or
//! `grype` on PATH) against the same database dir (`<xdg cache>/k9s/db`),
//! with k9s's options (auto CPEs, no user config, one DB load at startup).
//! Without grype, or when the DB fails to load, the scanner stays
//! uninitialized like k9s after a DB load failure: VS shows `n/a` and the
//! scans view is empty.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use serde_json::Value;

use crate::config::ImageScans;

/// Pinned grype release k9s v0.50.18 embeds.
pub const GRYPE_VERSION: &str = "0.104.2";

pub const SEV1: &str = "SEV-1";
pub const SEV2: &str = "SEV-2";
pub const SEV3: &str = "SEV-3";
pub const SEV4: &str = "SEV-4";
pub const SEV5: &str = "SEV-5";
pub const SEVU: &str = "SEV-U";

const WONT_FIX: &str = "(won't fix)";
const NA_VALUE: &str = "n/a";

const SEV_CRITICAL: usize = 0;
const SEV_HIGH: usize = 1;
const SEV_MEDIUM: usize = 2;
const SEV_LOW: usize = 3;
const SEV_NEGLIGIBLE: usize = 4;
const SEV_UNKNOWN: usize = 5;
const SEV_FIXED: usize = 6;

/// One finding: name, installed, fixed-in, type, vulnerability, severity.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Row(pub [String; 6]);

impl Row {
    fn new(name: &str, ver: &str, fix: &str, typ: &str, vul: &str, sev: &str) -> Row {
        Row([
            name.into(),
            ver.into(),
            fix.into(),
            typ.into(),
            vul.into(),
            to_sev(sev).into(),
        ])
    }
    pub fn name(&self) -> &str {
        &self.0[0]
    }
    pub fn version(&self) -> &str {
        &self.0[1]
    }
    pub fn fix(&self) -> &str {
        &self.0[2]
    }
    pub fn typ(&self) -> &str {
        &self.0[3]
    }
    pub fn vulnerability(&self) -> &str {
        &self.0[4]
    }
    pub fn severity(&self) -> &str {
        &self.0[5]
    }
}

/// vul.toSev.
fn to_sev(s: &str) -> &'static str {
    match s {
        "Critical" => SEV1,
        "High" => SEV2,
        "Medium" => SEV3,
        "Low" => SEV4,
        "Negligible" => SEV5,
        _ => SEVU,
    }
}

/// vul.tally: per severity counts plus fixable findings.
pub type Tally = [usize; 7];

fn tally(rows: &[Row]) -> Tally {
    let mut t = [0; 7];
    for r in rows {
        if !r.fix().is_empty() {
            t[SEV_FIXED] += 1;
        }
        let i = match r.severity() {
            SEV1 => SEV_CRITICAL,
            SEV2 => SEV_HIGH,
            SEV3 => SEV_MEDIUM,
            SEV4 => SEV_LOW,
            SEV5 => SEV_NEGLIGIBLE,
            _ => SEV_UNKNOWN,
        };
        t[i] += 1;
    }
    t
}

/// vul.fromTally: one bit per severity present.
fn score_bits(t: &Tally) -> u8 {
    const BITS: [u8; 6] = [0x80, 0x40, 0x20, 0x10, 0x08, 0x04];
    t.iter()
        .take(6)
        .zip(BITS)
        .filter(|(n, _)| **n > 0)
        .fold(0, |b, (_, bit)| b | bit)
}

/// scorer.String: the top six bits.
fn score_string(b: u8) -> String {
    format!("{b:08b}")[..6].to_string()
}

/// One image's findings (vul.Scan).
#[derive(Clone, Debug, Default)]
pub struct Scan {
    pub id: String,
    pub rows: Vec<Row>,
    pub tally: Tally,
}

impl Scan {
    /// Scan.run over grype's JSON report, then table.dedup and newTally.
    fn from_report(id: &str, report: &Value) -> Scan {
        let mut rows = Vec::new();
        for m in report
            .get("matches")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let s = |p: &str| m.pointer(p).and_then(Value::as_str).unwrap_or("");
            let versions: Vec<&str> = m
                .pointer("/vulnerability/fix/versions")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let fix = match s("/vulnerability/fix/state") {
                "wont-fix" => WONT_FIX.to_string(),
                "unknown" | "" => String::new(),
                _ => versions.join(", "),
            };
            rows.push(Row::new(
                s("/artifact/name"),
                s("/artifact/version"),
                &fix,
                s("/artifact/type"),
                s("/vulnerability/id"),
                s("/vulnerability/severity"),
            ));
        }
        let mut seen = HashSet::new();
        rows.retain(|r| seen.insert(r.clone()));
        let tally = tally(&rows);
        Scan {
            id: id.to_string(),
            rows,
            tally,
        }
    }
}

/// How the scanner talks to grype; tests swap in a fake.
pub trait Engine: Send + Sync {
    /// grype.LoadVulnerabilityDB with auto update.
    fn load_db(&self) -> anyhow::Result<()>;
    /// Scan an image; grype's JSON report.
    fn scan(&self, image: &str) -> anyhow::Result<Value>;
}

/// The grype CLI.
pub struct GrypeCli {
    pub bin: PathBuf,
    pub db_dir: PathBuf,
    pub config: PathBuf,
}

impl GrypeCli {
    /// `$K9RS_GRYPE`, else `grype` on PATH.
    pub fn locate(cache_home: &Path) -> Option<GrypeCli> {
        let bin = std::env::var_os("K9RS_GRYPE")
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .or_else(|| which::which("grype").ok())?;
        // Own cache dir: never shares (clobbers) k9s's scanner files.
        let root = cache_home.join("k9rs");
        Some(GrypeCli {
            bin,
            db_dir: root.join("db"),
            config: root.join("k9rs-grype.yaml"),
        })
    }

    fn cmd(&self, auto_update: bool) -> std::process::Command {
        let mut c = std::process::Command::new(&self.bin);
        c.env("GRYPE_DB_CACHE_DIR", &self.db_dir)
            .env(
                "GRYPE_DB_AUTO_UPDATE",
                if auto_update { "true" } else { "false" },
            )
            .env("GRYPE_CHECK_FOR_APP_UPDATE", "false")
            .arg("-c")
            .arg(&self.config)
            .stdin(std::process::Stdio::null());
        c
    }

    fn run(mut c: std::process::Command) -> anyhow::Result<Vec<u8>> {
        let out = c.output()?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!("{}", err.trim().lines().last().unwrap_or("grype failed"));
        }
        Ok(out.stdout)
    }
}

impl Engine for GrypeCli {
    fn load_db(&self) -> anyhow::Result<()> {
        if let Some(d) = self.config.parent() {
            std::fs::create_dir_all(d)?;
        }
        // No user ~/.grype.yaml: k9s builds its options from defaults.
        std::fs::write(&self.config, "check-for-app-update: false\n")?;
        let version = Self::run({
            let mut c = self.cmd(false);
            c.arg("version").arg("-o").arg("json");
            c
        })?;
        let v: Value = serde_json::from_slice(&version)?;
        let got = v.get("version").and_then(Value::as_str).unwrap_or("");
        if got != GRYPE_VERSION {
            tracing::warn!(
                version = got,
                want = GRYPE_VERSION,
                "grype version differs from k9rs"
            );
        }
        // An update failure is only a warning (RequireUpdateCheck is off).
        let mut up = self.cmd(true);
        up.arg("db").arg("update");
        if let Err(e) = Self::run(up) {
            tracing::warn!(error = %e, "error updating db");
        }
        let mut st = self.cmd(false);
        st.arg("db").arg("status").arg("-o").arg("json");
        let status: Value = serde_json::from_slice(&Self::run(st)?)?;
        if status.get("valid").and_then(Value::as_bool) != Some(true) {
            anyhow::bail!(
                "db could not be loaded: {}",
                status
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("invalid db")
            );
        }
        Ok(())
    }

    fn scan(&self, image: &str) -> anyhow::Result<Value> {
        let mut c = self.cmd(false);
        // GenerateMissingCPEs.
        c.arg(image).arg("-o").arg("json").arg("--add-cpes-if-none");
        Ok(serde_json::from_slice(&Self::run(c)?)?)
    }
}

/// No grype binary: the DB never loads (like a k9s DB load failure).
struct NoGrype;

impl Engine for NoGrype {
    fn load_db(&self) -> anyhow::Result<()> {
        anyhow::bail!("grype {GRYPE_VERSION} not found (set K9RS_GRYPE or add grype to PATH)")
    }
    fn scan(&self, _image: &str) -> anyhow::Result<Value> {
        anyhow::bail!("no vulnerability db")
    }
}

/// The scanner k9s builds for `imageScans` (grype CLI, else none).
pub fn new_for(cfg: ImageScans, cache_home: &Path) -> ImageScanner {
    let engine: Arc<dyn Engine> = match GrypeCli::locate(cache_home) {
        Some(g) => Arc::new(g),
        None => Arc::new(NoGrype),
    };
    ImageScanner::new(cfg, engine)
}

/// vul.imageScanner.
pub struct ImageScanner {
    config: ImageScans,
    engine: Arc<dyn Engine>,
    scans: RwLock<HashMap<String, Arc<RwLock<Scan>>>>,
    initialized: AtomicBool,
}

impl ImageScanner {
    pub fn new(config: ImageScans, engine: Arc<dyn Engine>) -> ImageScanner {
        ImageScanner {
            config,
            engine,
            scans: RwLock::new(HashMap::new()),
            initialized: AtomicBool::new(false),
        }
    }

    /// Init: load the DB; stays uninitialized on failure.
    pub fn init(&self) {
        match self.engine.load_db() {
            Ok(()) => {
                self.initialized.store(true, Ordering::SeqCst);
                tracing::debug!("VulDB initialized");
            }
            Err(e) => tracing::error!(error = %e, "VulDb load failed"),
        }
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::SeqCst)
    }

    pub fn should_exclude(&self, ns: &str, ll: &BTreeMap<String, String>) -> bool {
        self.config.should_exclude(ns, ll)
    }

    pub fn get_scan(&self, img: &str) -> Option<Scan> {
        let m = self.scans.read().ok()?;
        let s = m.get(img)?;
        s.read().ok().map(|s| s.clone())
    }

    /// Enqueue: scan images not seen yet (the empty scan is recorded first).
    pub fn enqueue(self: &Arc<Self>, images: &[String]) {
        for img in images {
            let slot = {
                let Ok(mut m) = self.scans.write() else {
                    return;
                };
                if m.contains_key(img) {
                    continue;
                }
                let slot = Arc::new(RwLock::new(Scan {
                    id: img.clone(),
                    ..Default::default()
                }));
                m.insert(img.clone(), slot.clone());
                slot
            };
            let (engine, img) = (self.engine.clone(), img.clone());
            let work = move || match engine.scan(&img) {
                Ok(report) => {
                    if let Ok(mut s) = slot.write() {
                        *s = Scan::from_report(&img, &report);
                    }
                }
                Err(e) => {
                    tracing::warn!(subsys = "vul", image = %img, error = %e, "Scan failed for image")
                }
            };
            match tokio::runtime::Handle::try_current() {
                Ok(h) => {
                    h.spawn_blocking(work);
                }
                Err(_) => {
                    std::thread::spawn(work);
                }
            }
        }
    }

    /// Score: OR of the images' severity bits.
    pub fn score(&self, images: &[String]) -> String {
        let b = images
            .iter()
            .filter_map(|i| self.get_scan(i))
            .fold(0u8, |b, s| b | score_bits(&s.tally));
        score_string(b)
    }
}

fn global() -> &'static RwLock<Option<Arc<ImageScanner>>> {
    static S: OnceLock<RwLock<Option<Arc<ImageScanner>>>> = OnceLock::new();
    S.get_or_init(|| RwLock::new(None))
}

/// vul.ImgScanner.
pub fn scanner() -> Option<Arc<ImageScanner>> {
    global().read().ok().and_then(|g| g.clone())
}

/// App.initImgScanner: install the scanner and load the DB in the
/// background.
pub fn install(s: Arc<ImageScanner>) {
    if let Ok(mut g) = global().write() {
        *g = Some(s.clone());
    }
    std::thread::spawn(move || s.init());
}

/// Remove the scanner (tests).
pub fn uninstall() {
    if let Ok(mut g) = global().write() {
        *g = None;
    }
}

/// render.ExtractImages: the regular containers' images.
pub fn extract_images(spec: &Value) -> Vec<String> {
    spec.get("containers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|c| {
            c.get("image")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        })
        .collect()
}

/// render.computeVulScore.
pub fn compute_vul_score(ns: &str, labels: &BTreeMap<String, String>, spec: &Value) -> String {
    let Some(s) = scanner() else {
        return NA_VALUE.into();
    };
    if !s.is_initialized() || s.should_exclude(ns, labels) {
        return NA_VALUE.into();
    }
    let ii = extract_images(spec);
    s.enqueue(&ii);
    s.score(&ii)
}

/// vul severity colors for the scans table (render.ImageScan.ColorerFunc).
pub fn sev_rank(s: &str) -> usize {
    match s {
        SEV1 => 1,
        SEV2 => 2,
        SEV3 => 3,
        SEV4 => 4,
        SEV5 => 5,
        _ => 6,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) struct FakeEngine {
        pub ok: bool,
        pub report: Value,
    }

    impl Engine for FakeEngine {
        fn load_db(&self) -> anyhow::Result<()> {
            if self.ok {
                Ok(())
            } else {
                anyhow::bail!("no db")
            }
        }
        fn scan(&self, _image: &str) -> anyhow::Result<Value> {
            Ok(self.report.clone())
        }
    }

    pub(crate) fn report() -> Value {
        json!({"matches": [
            {"artifact": {"name": "libcrypto3", "version": "3.1.0-r4", "type": "apk"},
             "vulnerability": {"id": "CVE-1", "severity": "High",
                "fix": {"versions": ["3.1.7-r0", "3.1.8"], "state": "fixed"}}},
            {"artifact": {"name": "libcrypto3", "version": "3.1.0-r4", "type": "apk"},
             "vulnerability": {"id": "CVE-1", "severity": "High",
                "fix": {"versions": ["3.1.7-r0", "3.1.8"], "state": "fixed"}}},
            {"artifact": {"name": "busybox", "version": "1.36", "type": "apk"},
             "vulnerability": {"id": "GHSA-x", "severity": "Low",
                "fix": {"versions": [], "state": "wont-fix"}}},
            {"artifact": {"name": "zlib", "version": "1", "type": "apk"},
             "vulnerability": {"id": "CVE-2", "severity": "",
                "fix": {"versions": ["2"], "state": "unknown"}}}
        ]})
    }

    #[test]
    fn report_rows_dedup_tally_and_score() {
        let s = Scan::from_report("alpine", &report());
        assert_eq!(s.rows.len(), 3);
        assert_eq!(
            s.rows[0].0,
            [
                "libcrypto3",
                "3.1.0-r4",
                "3.1.7-r0, 3.1.8",
                "apk",
                "CVE-1",
                SEV2
            ]
            .map(String::from)
        );
        assert_eq!(s.rows[1].fix(), WONT_FIX);
        assert_eq!(s.rows[2].fix(), "");
        assert_eq!(s.rows[2].severity(), SEVU);
        assert_eq!(s.tally, [0, 1, 0, 1, 0, 1, 2]);
        assert_eq!(score_string(score_bits(&s.tally)), "010101");
        assert_eq!(score_string(0), "000000");
        assert_eq!(score_string(0x80 | 0x04), "100001");
    }

    #[test]
    fn uninitialized_or_excluded_is_na() {
        let mut cfg = ImageScans {
            enable: true,
            ..Default::default()
        };
        cfg.exclusions.namespaces = vec!["skip".into()];
        let bad = ImageScanner::new(
            cfg.clone(),
            Arc::new(FakeEngine {
                ok: false,
                report: report(),
            }),
        );
        bad.init();
        assert!(!bad.is_initialized());
        let good = Arc::new(ImageScanner::new(
            cfg,
            Arc::new(FakeEngine {
                ok: true,
                report: report(),
            }),
        ));
        good.init();
        assert!(good.is_initialized());
        assert!(good.should_exclude("skip", &BTreeMap::new()));
        let ii = vec!["alpine".to_string()];
        // The empty scan is recorded first, so the score starts at zero.
        assert_eq!(good.score(&ii), "000000");
        good.enqueue(&ii);
        for _ in 0..100 {
            if good.get_scan("alpine").is_some_and(|s| !s.rows.is_empty()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(good.score(&ii), "010101");
    }

    #[test]
    fn images_from_regular_containers_only() {
        let spec = json!({"initContainers": [{"image": "init"}],
            "containers": [{"image": "a"}, {"image": "b"}]});
        assert_eq!(extract_images(&spec), vec!["a", "b"]);
    }
}
