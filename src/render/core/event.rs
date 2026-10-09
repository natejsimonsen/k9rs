//! Event renderer (k9s render/ev.go). k9s renders events from the server-side
//! table (`is_generic()` is true, so callers should use
//! `render_server_table`). When only raw objects are available, this
//! renderer reproduces the server's event table (kubectl printEvent, wide)
//! for both `v1` and `events.k8s.io/v1` shapes:
//! LAST SEEN, TYPE, REASON, OBJECT, SUBOBJECT, SOURCE, MESSAGE, FIRST SEEN,
//! COUNT, NAME (plus NAMESPACE when cluster wide).

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::config::ViewSetting;
use crate::model::{Header, HeaderColumn, Row};
use crate::render::helpers::*;
use crate::render::{RenderInput, Renderer};

pub(crate) fn header() -> Header {
    Header::new(vec![
        HeaderColumn::new("NAMESPACE"),
        HeaderColumn::new("LAST SEEN").time(),
        HeaderColumn::new("TYPE"),
        HeaderColumn::new("REASON"),
        HeaderColumn::new("OBJECT"),
        HeaderColumn::new("SUBOBJECT"),
        HeaderColumn::new("SOURCE"),
        HeaderColumn::new("MESSAGE"),
        HeaderColumn::new("FIRST SEEN").time(),
        HeaderColumn::new("COUNT"),
        HeaderColumn::new("NAME"),
    ])
}

/// k9s render.Event.
#[derive(Default)]
pub struct Event {
    base: ViewBase,
}

/// First non-empty string among the given top-level fields.
fn first<'a>(o: &'a kube::api::DynamicObject, keys: &[&str]) -> &'a str {
    keys.iter()
        .map(|k| data_str(o, k))
        .find(|s| !s.is_empty())
        .unwrap_or("")
}

fn time_of(o: &kube::api::DynamicObject, keys: &[&str]) -> Option<DateTime<Utc>> {
    keys.iter().find_map(|k| value_time(data_get(o, k)))
}

impl Renderer for Event {
    fn is_generic(&self) -> bool {
        true
    }

    fn header(&self, ns: &str) -> Header {
        self.base.header(&header(), ns)
    }

    fn render(&self, input: &RenderInput<'_>, ns: &str, row: &mut Row) -> Result<()> {
        let o = input.obj;
        let null = Value::Null;
        let involved = o
            .data
            .get("involvedObject")
            .or_else(|| o.data.get("regarding"))
            .unwrap_or(&null);
        let source = o
            .data
            .get("source")
            .or_else(|| o.data.get("deprecatedSource"))
            .unwrap_or(&null);

        let first_ts = match time_of(o, &["firstTimestamp", "deprecatedFirstTimestamp"]) {
            Some(t) => time_to_age(t),
            None => to_age_opt(time_of(o, &["eventTime"])),
        };
        let mut last_ts = match time_of(o, &["lastTimestamp", "deprecatedLastTimestamp"]) {
            Some(t) => time_to_age(t),
            None => first_ts.clone(),
        };
        let mut count = o
            .data
            .get("count")
            .or_else(|| o.data.get("deprecatedCount"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if let Some(series) = o.data.get("series").filter(|s| !s.is_null()) {
            last_ts = to_age_opt(value_time(series.get("lastObservedTime")));
            count = i64_at(series, "count");
        } else if count == 0 {
            count = 1;
        }
        let kind = str_at(involved, "kind").to_lowercase();
        let iname = str_at(involved, "name");
        let target = if iname.is_empty() {
            kind
        } else {
            format!("{kind}/{iname}")
        };
        let component = match str_at(source, "component") {
            "" => first(o, &["reportingController", "reportingComponent"]),
            c => c,
        };
        let instance = match str_at(source, "host") {
            "" => data_str(o, "reportingInstance"),
            h => h,
        };
        let src = if instance.is_empty() {
            component.to_string()
        } else {
            format!("{component}, {instance}")
        };

        row.id = obj_fqn(o);
        row.fields = vec![
            obj_ns(o).to_string(),
            last_ts,
            data_str(o, "type").to_string(),
            data_str(o, "reason").to_string(),
            target,
            str_at(involved, "fieldPath").to_string(),
            src,
            first(o, &["message", "note"]).trim().to_string(),
            first_ts,
            count.to_string(),
            obj_name(o).to_string(),
        ];
        let raw = obj_to_value(o);
        self.base.finish(Some(&raw), &header(), ns, row)
    }

    fn set_view_setting(&mut self, vs: Option<ViewSetting>) {
        self.base.set_view_setting(vs.as_ref());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::core::testutil::*;
    use serde_json::json;

    #[test]
    fn core_v1_event() {
        let o = load("ev");
        let e = Event::default();
        let mut r = Row::default();
        let n = parse_time("2019-08-30T20:48:05Z").unwrap();
        with_now(n, || e.render(&RenderInput::new(&o), "all", &mut r)).unwrap();
        assert_eq!(r.id, "default/hello-1567197780-mn4mv.15bfce150bd764dd");
        assert_eq!(
            r.fields,
            [
                "default",
                "5m",
                "Normal",
                "Pulled",
                "pod/hello-1567197780-mn4mv",
                "spec.containers{c1}",
                "kubelet, gke-k9s-default-pool-0fa2fb89-qnkc",
                "Successfully pulled image \"blang/busybox-bash\"",
                "5m",
                "1",
                "hello-1567197780-mn4mv.15bfce150bd764dd"
            ]
        );
        assert_eq!(e.header("all").len(), 11);
        assert_eq!(e.header("default").len(), 10);
    }

    #[test]
    fn events_k8s_io_event() {
        let o = obj(json!({
            "apiVersion": "events.k8s.io/v1", "kind": "Event",
            "metadata": {"name": "e1", "namespace": "ns1"},
            "eventTime": "2019-12-31T23:00:00.000000Z",
            "series": {"count": 4, "lastObservedTime": "2019-12-31T23:59:00.000000Z"},
            "regarding": {"kind": "Node", "name": ""},
            "reportingController": "ctrl", "reportingInstance": "inst",
            "note": "  hello  ", "reason": "Boom", "type": "Warning"
        }));
        let mut r = Row::default();
        with_now(t0(), || {
            Event::default().render(&RenderInput::new(&o), "ns1", &mut r)
        })
        .unwrap();
        assert_eq!(
            r.fields,
            [
                "60s",
                "Warning",
                "Boom",
                "node",
                "",
                "ctrl, inst",
                "hello",
                "60m",
                "4",
                "e1"
            ]
        );
        let o = obj(
            json!({"apiVersion": "v1", "kind": "Event", "metadata": {"name": "e2", "namespace": "ns1"}}),
        );
        Event::default()
            .render(&RenderInput::new(&o), "ns1", &mut r)
            .unwrap();
        assert_eq!(r.fields[0], "<unknown>");
        assert_eq!(r.fields[7], "<unknown>");
        assert_eq!(r.fields[8], "1");
    }
}
