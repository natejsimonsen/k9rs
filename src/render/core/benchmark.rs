//! Benchmark renderer (k9s render/benchmark.go). Input: pseudo `Benchmark`;
//! the report file at `path` is read when rendering, like k9s.

use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use ratatui::style::Color;
use regex::Regex;

use super::PALE_GREEN;
use crate::model::{Align, Header, HeaderColumn, Row, RowEvent};
use crate::render::helpers::{as_thousands, obj_name, to_age_opt, value_time, ViewBase};
use crate::render::pseudo::str_field;
use crate::render::{is_valid, Palette, RenderInput, Renderer};

struct Rx {
    total: Regex,
    req: Regex,
    ok: Regex,
    err: Regex,
    toast: Regex,
}

fn rx() -> &'static Rx {
    static RX: OnceLock<Rx> = OnceLock::new();
    RX.get_or_init(|| Rx {
        total: Regex::new(r"Total:\s+([0-9.]+)\ssecs").expect("regex"),
        req: Regex::new(r"Requests/sec:\s+([0-9.]+)").expect("regex"),
        ok: Regex::new(r"\[2\d{2}\]\s+(\d+)\s+responses").expect("regex"),
        err: Regex::new(r"\[[45]\d{2}\]\s+(\d+)\s+responses").expect("regex"),
        toast: Regex::new(r"Error distribution").expect("regex"),
    })
}

pub(crate) fn header() -> Header {
    let r = |n: &str| HeaderColumn::new(n).align(Align::Right);
    Header::new(vec![
        HeaderColumn::new("NAMESPACE"),
        HeaderColumn::new("NAME"),
        HeaderColumn::new("STATUS"),
        HeaderColumn::new("TIME"),
        r("REQ/S"),
        r("2XX"),
        r("4XX/5XX"),
        HeaderColumn::new("REPORT"),
        HeaderColumn::new("VALID").wide(),
        HeaderColumn::new("AGE").time(),
    ])
}

/// k9s render.Benchmark.
pub struct Benchmark;

impl Renderer for Benchmark {
    fn header(&self, ns: &str) -> Header {
        ViewBase::default().header(&header(), ns)
    }

    fn render(&self, input: &RenderInput<'_>, ns: &str, row: &mut Row) -> Result<()> {
        let o = input.obj;
        let path = str_field(o, "path");
        let data = std::fs::read_to_string(path)
            .map_err(|_| anyhow!("unable to load bench file {path}"))?;
        let file = obj_name(o);
        let tokens: Vec<&str> = file.split('_').collect();
        if tokens.len() < 2 {
            return Err(anyhow!("invalid file name {file}"));
        }
        let mut ff = vec![String::new(); header().len()];
        ff[0] = tokens[0].to_string();
        ff[1] = tokens[1].to_string();
        ff[7] = file.to_string();
        ff[9] = to_age_opt(value_time(o.data.get("modified")));
        augment_row(&mut ff, &data);
        ff[8] = diagnose(ns, &ff);
        row.id = path.to_string();
        row.fields = ff;
        ViewBase::default().finish(None, &header(), ns, row)
    }

    fn color_for(&self, _ns: &str, h: &Header, re: &RowEvent, p: &Palette) -> Color {
        if !is_valid(h, &re.row) {
            return p.err;
        }
        PALE_GREEN
    }
}

/// k9s diagnose. Note k9s checks the TIME column (index 3) when the
/// namespace is cluster wide, so cluster wide rows always report a failure;
/// this is reproduced as is.
fn diagnose(ns: &str, ff: &[String]) -> String {
    let mut status_col = 3;
    if !crate::k8s::is_all_namespaces(ns) {
        status_col -= 1;
    }
    if ff.len() < status_col {
        return String::new();
    }
    if ff[status_col] != "pass" {
        return "failed benchmark".into();
    }
    String::new()
}

fn augment_row(fields: &mut [String], data: &str) {
    if data.is_empty() {
        return;
    }
    let rx = rx();
    let mut col = 2;
    fields[col] = if rx.toast.is_match(data) {
        "fail"
    } else {
        "pass"
    }
    .into();
    col += 1;
    if let Some(m) = rx.total.captures(data) {
        fields[col] = m[1].to_string();
    }
    col += 1;
    if let Some(m) = rx.req.captures(data) {
        fields[col] = m[1].to_string();
    }
    col += 1;
    fields[col] = count_req(&rx.ok, data);
    col += 1;
    fields[col] = count_req(&rx.err, data);
}

fn count_req(rx: &Regex, data: &str) -> String {
    let mut n = 0;
    let mut sum: i64 = 0;
    for m in rx.captures_iter(data) {
        n += 1;
        if let Ok(v) = m[1].parse::<i64>() {
            sum += v;
        }
    }
    if n == 0 {
        return "0".into();
    }
    as_thousands(sum)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ResEvent;
    use crate::render::core::testutil::*;
    use crate::render::helpers::with_now;

    fn td(f: &str) -> String {
        format!(
            "{}/src/render/core/testdata/{f}",
            env!("CARGO_MANIFEST_DIR")
        )
    }

    #[test]
    fn augment() {
        let cases = [
            ("b1.txt", ["pass", "3.3544", "29.8116", "100", "0"]),
            ("b4.txt", ["pass", "3.3544", "29.8116", "160", "0"]),
            ("b2.txt", ["pass", "3.3544", "29.8116", "100", "12"]),
            ("b3.txt", ["fail", "2.3688", "35.4606", "0", "0"]),
        ];
        for (f, e) in cases {
            let data = std::fs::read_to_string(td(f)).unwrap();
            let mut ff = vec![String::new(); 8];
            augment_row(&mut ff, &data);
            assert_eq!(ff[2..7], e, "{f}");
        }
    }

    #[test]
    fn render_bench() {
        let dir = tempfile::tempdir().unwrap();
        let file = "default_nginx_1234.txt";
        let path = dir.path().join(file);
        std::fs::copy(td("b1.txt"), &path).unwrap();
        let p = path.to_string_lossy().to_string();
        let o = crate::render::pseudo::benchmark(
            "default",
            "po",
            &p,
            file,
            t0() - chrono::Duration::seconds(30),
        );
        let mut r = Row::default();
        with_now(t0(), || {
            Benchmark.render(&RenderInput::new(&o), "default", &mut r)
        })
        .unwrap();
        assert_eq!(r.id, p);
        assert_eq!(
            r.fields,
            ["nginx", "pass", "3.3544", "29.8116", "100", "0", file, "", "30s"]
        );
        assert_eq!(r.fields.len(), Benchmark.header("default").len());
        let h = Benchmark.header("default");
        assert_eq!(
            Benchmark.color_for("", &h, &RowEvent::new(ResEvent::Add, r.clone()), &palette()),
            PALE_GREEN
        );

        // k9s quirk: all-namespaces rows check the TIME column.
        Benchmark
            .render(&RenderInput::new(&o), "all", &mut r)
            .unwrap();
        assert_eq!(r.fields[0], "default");
        assert_eq!(r.fields[8], "failed benchmark");
        let h = Benchmark.header("all");
        assert_eq!(
            Benchmark.color_for("", &h, &RowEvent::new(ResEvent::Add, r), &palette()),
            palette().err
        );
    }

    #[test]
    fn render_errors() {
        let o = crate::render::pseudo::benchmark("default", "po", "/nope/nope", "a_b.txt", t0());
        let mut r = Row::default();
        let e = Benchmark
            .render(&RenderInput::new(&o), "", &mut r)
            .unwrap_err();
        assert_eq!(e.to_string(), "unable to load bench file /nope/nope");
        let p = td("b1.txt");
        let o = crate::render::pseudo::benchmark("default", "po", &p, "nounderscore", t0());
        let e = Benchmark
            .render(&RenderInput::new(&o), "", &mut r)
            .unwrap_err();
        assert_eq!(e.to_string(), "invalid file name nounderscore");
    }
}
