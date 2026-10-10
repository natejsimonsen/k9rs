//! DescribeEvents: the trailing `Events:` table.

use serde_json::Value;

use super::gosort::sort_by;
use super::util::{parse_time, Ctx, J};
use super::writer::{PrefixWriter, L0, L1};

pub(crate) fn describe_events(ctx: &Ctx, w: &mut PrefixWriter, events: &[Value]) {
    if events.is_empty() {
        w.write(L0, "Events:\t<none>\n");
        return;
    }
    w.flush();
    let mut evs: Vec<J> = events.iter().map(J).collect();
    // SortableEvents: by lastTimestamp (unset sorts as the zero time).
    sort_by(&mut evs, |a, b| {
        let (ta, tb) = (
            parse_time(a.get("lastTimestamp")),
            parse_time(b.get("lastTimestamp")),
        );
        match (ta, tb) {
            (None, Some(_)) => true,
            (Some(x), Some(y)) => x < y,
            _ => false,
        }
    });
    w.write(L0, "Events:\n  Type\tReason\tAge\tFrom\tMessage\n");
    w.write(L1, "----\t------\t----\t----\t-------\n");
    for e in evs {
        let first = if e.exists("eventTime") && parse_time(e.get("eventTime")).is_some() {
            ctx.since(e.get("eventTime"))
        } else {
            ctx.since(e.get("firstTimestamp"))
        };
        let count = e.i("count");
        let interval = if e.exists("series") {
            format!(
                "{} (x{} over {})",
                ctx.since(e.get("series.lastObservedTime")),
                e.i("series.count"),
                first
            )
        } else if count > 1 {
            format!(
                "{} (x{} over {})",
                ctx.since(e.get("lastTimestamp")),
                count,
                first
            )
        } else {
            first
        };
        let mut source = e.s("source.component");
        if source.is_empty() {
            source = e.s("reportingComponent");
        }
        // kubectl v0.35 prefixes the involved object's field path.
        let mut message = e.s("message").trim().to_string();
        let fp = e.s("involvedObject.fieldPath");
        if !fp.is_empty() {
            message = format!("{fp}: {message}");
        }
        w.write(
            L1,
            &format!(
                "{}\t{}\t{}\t{}\t{}\n",
                e.s("type"),
                e.s("reason"),
                interval,
                source,
                message
            ),
        );
    }
}
