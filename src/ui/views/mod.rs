//! Non-table views (owned by the `ui-details` slice): describe/yaml details,
//! logs, xray, pulses, text, and the port-forward benchmark runner.

pub(crate) mod bench;
mod details;
mod logs;
mod pulses;
mod util;
mod xray;

use crate::k8s::{Gvr, LogOptions};

use super::{Ctx, View};

/// `d`: describe `path` of `gvr`.
pub fn describe(ctx: &mut Ctx, gvr: Gvr, path: String) -> Box<dyn View> {
    Box::new(details::DetailsView::live(
        ctx, "Describe", gvr, path, false,
    ))
}
/// `y`: yaml of `path`.
pub fn yaml(ctx: &mut Ctx, gvr: Gvr, path: String) -> Box<dyn View> {
    Box::new(details::DetailsView::live(
        ctx,
        details::YAML_ACTION,
        gvr,
        path,
        true,
    ))
}
/// `l`/`p`: logs per `opts`.
pub fn logs(ctx: &mut Ctx, opts: LogOptions) -> Box<dyn View> {
    Box::new(logs::LogsView::new(ctx, opts))
}
/// `x` / `:xray <res> [ns]`.
pub fn xray(ctx: &mut Ctx, gvr: Gvr, ns: String) -> Box<dyn View> {
    Box::new(xray::XrayView::new(ctx, gvr, ns))
}
/// `:pulses`.
pub fn pulses(ctx: &mut Ctx, ns: String) -> Box<dyn View> {
    Box::new(pulses::PulseView::new(ctx, ns))
}
/// Generic scrollable text view (plugin output, file contents, `:cow`).
/// `name` is the frame title word (k9s Details title, e.g. `Results`) and
/// `title` the subject shown in parens; `name == "cow"` renders the k9s cow
/// error view with `body` as its message.
pub fn text(ctx: &mut Ctx, name: String, title: String, body: String) -> Box<dyn View> {
    if name == "cow" {
        return Box::new(details::DetailsView::cow(ctx, &body));
    }
    // NewDetails(.., contentYAML, ..): one-shot YAML and the views k9s
    // opens with YAML content get YAML colors.
    let yaml = matches!(
        name.as_str(),
        "YAML" | "Secret Decoder" | "Drain Progress" | "Applied Manifest" | "Deleted Manifest"
    );
    Box::new(details::DetailsView::fixed(
        ctx, &name, title, yaml, true, &body,
    ))
}

pub(crate) use util::{print_markup, print_tags};

/// Helm release notes/manifest in a live view (helm dao Describe/ToYAML).
pub fn helm_release(ctx: &mut Ctx, title: String, subject: String, body: String) -> Box<dyn View> {
    Box::new(details::DetailsView::release(ctx, &title, subject, &body))
}

/// Helm values in a live view (view.showValues / History.getValsCmd).
pub fn helm_values(ctx: &mut Ctx, subject: String, user: String, all: String) -> Box<dyn View> {
    Box::new(details::DetailsView::values(ctx, subject, user, all))
}

/// Whether `:xray` supports `gvr` (k9s allowedXRay).
pub fn xray_supported(gvr: &Gvr) -> bool {
    xray::allowed(gvr)
}

/// Benchmark settings for one port forward (k9s config.BenchConfig).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BenchSpec {
    pub url: String,
    pub method: String,
    pub concurrency: usize,
    pub requests: usize,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub user: Option<(String, String)>,
    pub http2: bool,
}

/// Run a benchmark (k9s perf.Benchmark, a hey-style load run) and write the
/// report to the k9s benchmarks dir. Returns the report path.
pub async fn run_benchmark(
    spec: BenchSpec,
    cluster: String,
    context: String,
    name: String,
) -> anyhow::Result<std::path::PathBuf> {
    bench::run_into(spec, &bench::benchmarks_dir(), &cluster, &context, &name).await
}
