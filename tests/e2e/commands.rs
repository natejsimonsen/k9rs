//! `:` commands resolved through the alias table into resource views.

use crate::common::*;

fn header(s: &Screen) -> Vec<String> {
    s.line(1)
        .split_whitespace()
        .filter(|w| *w != "│")
        .map(String::from)
        .collect()
}

/// render/dp.go header; DefaultColorer (add on first load, std after),
/// invalid rows (not ready) are ErrColor.
#[tokio::test]
async fn deploy_view() {
    let mut h = Harness::new().await;
    h.goto("deploy").await.unwrap();
    assert_eq!(h.top().title(), " deployments(default)[2] ");
    assert_eq!(h.top().name(), "Deployment");
    let s = h.render();
    assert_eq!(
        header(&s),
        ["NAME↑", "READY", "UP-TO-DATE", "AVAILABLE", "AGE"]
    );
    let api = s.line(s.row_of("api").unwrap());
    assert!(api.contains("0/1"), "{api}");
    let nginx = s.line(s.row_of("nginx").unwrap());
    assert!(nginx.contains("2/2"), "{nginx}");
    // Cursor row: bg = row color (orangered: not ready), fg = black.
    assert_eq!(s.bg_of("api"), color("orangered"));
    assert_eq!(s.fg_of("api"), color("black"));
    assert_eq!(s.fg_of("nginx"), color("dodgerblue"), "new row = addColor");
    h.tick().await;
    let s = h.render();
    assert_eq!(
        s.fg_of("nginx"),
        color("lightskyblue"),
        "unchanged = newColor"
    );
}

/// render/svc.go ToPorts: `name:port►nodePort`.
#[tokio::test]
async fn svc_view() {
    let mut h = Harness::new().await;
    h.goto("svc").await.unwrap();
    assert_eq!(h.top().title(), " services(default)[2] ");
    let s = h.render();
    assert_eq!(
        header(&s),
        ["NAME↑", "TYPE", "CLUSTER-IP", "EXTERNAL-IP", "PORTS", "AGE"]
    );
    let k = s.line(s.row_of("kubernetes").unwrap());
    assert!(k.contains("ClusterIP") && k.contains("10.96.0.1") && k.contains("https:443►0"));
    let n = s.line(s.row_of("nginx").unwrap());
    assert!(n.contains("http:80►0"), "{n}");
}

/// Cluster scoped resources print `(all)` (ui/table.go styleTitle,
/// client.IsClusterWide). render/node.go header with metrics.
#[tokio::test]
async fn nodes_view() {
    let mut h = Harness::new().await;
    h.goto("no").await.unwrap();
    assert_eq!(h.top().title(), " nodes(all)[2] ");
    let s = h.render();
    let hdr = header(&s);
    assert_eq!(
        &hdr[..10],
        ["NAME↑", "STATUS", "ROLE", "TAINTS", "VERSION", "PODS", "CPU", "CPU/A", "%CPU", "MEM"]
    );
    let n1 = s.line(s.row_of("node-1").unwrap());
    for c in ["Ready", "worker", "v1.34.0", "1200", "4000", "30"] {
        assert!(n1.contains(c), "{c} missing: {n1}");
    }
}

/// render/ns.go: the `all` pseudo row, `+` favorite and `(*)` active marks.
#[tokio::test]
async fn namespaces_view() {
    let mut h = Harness::new().await;
    h.goto("ns").await.unwrap();
    assert_eq!(h.top().title(), " namespaces(all)[4] ");
    let s = h.render();
    let names: Vec<String> = s
        .table_names("NAME↑")
        .iter()
        .map(|r| r.split_whitespace().next().unwrap().to_string())
        .collect();
    assert_eq!(names, ["all", "default+(*)", "kube-system", "monitoring"]);
}

/// render/context.go: active context gets `(*)`.
#[tokio::test]
async fn contexts_view() {
    let mut h = Harness::new().await;
    h.goto("ctx").await.unwrap();
    assert_eq!(h.top().title(), " contexts(all)[2] ");
    let s = h.render();
    assert_eq!(header(&s), ["NAME↑", "CLUSTER", "AUTHINFO", "NAMESPACE"]);
    let names: Vec<String> = s
        .table_names("NAME↑")
        .iter()
        .map(|r| r.split_whitespace().next().unwrap().to_string())
        .collect();
    assert_eq!(names, ["fake(*)", "staging"]);
}

/// ctrl-a / `:aliases` (view/alias.go): RESOURCE, GROUP, VERSION, COMMAND.
#[tokio::test]
async fn aliases_view() {
    let mut h = Harness::new().await;
    for line in ["a", "alias", "aliases"] {
        assert_eq!(
            k9rs::cmd::parse(line, &h.ctx.aliases).unwrap().gvr,
            "aliases"
        );
    }
    h.goto("aliases").await.unwrap();
    assert!(
        h.top().title().starts_with(" aliases(all)["),
        "{}",
        h.top().title()
    );
    let s = h.render();
    assert_eq!(header(&s), ["RESOURCE↑", "GROUP", "VERSION", "COMMAND"]);
    let dp = s.line(s.row_of("deployments ").unwrap());
    assert!(dp.contains("apps") && dp.contains("deploy"), "{dp}");
    assert!(s.contains("widgets"), "CRD aliases are listed");
}

/// Other resources resolve through discovery short names.
#[tokio::test]
async fn more_resource_views() {
    let mut h = Harness::new().await;
    for (cmd, title, row) in [
        ("rs", " replicasets(default)[2] ", "nginx-7d9c5b8f4"),
        ("sts", " statefulsets(default)[1] ", "web"),
        ("cm", " configmaps(default)[1] ", "app-config"),
        ("secrets", " secrets(default)[1] ", "db-creds"),
        ("jobs", " jobs(default)[1] ", "migrate"),
        ("cj", " cronjobs(default)[1] ", "backup"),
        ("ev", " events(default)[1] ", "api-6f5d8c7b9-xyz12.17a"),
        ("crd", " customresourcedefinitions(all)[1] ", "widgets"),
        ("wdg", " widgets(default)[1] ", "my-widget"),
        ("ds kube-system", " daemonsets(kube-system)[1] ", "fluentd"),
    ] {
        h.goto(cmd).await.unwrap();
        assert_eq!(h.top().title(), title, "{cmd}");
        assert!(h.render().contains(row), "{cmd}: {row} missing");
    }
}

/// view/command.go: unknown commands error out (shown in the error dialog).
#[tokio::test]
async fn unknown_command_errors() {
    let h = Harness::new().await;
    let e = k9rs::cmd::parse("blah", &h.ctx.aliases).unwrap_err();
    assert_eq!(e.to_string(), "`blah` command not found");
}

/// `:q` / `:quit` / `:q!` map to the quit command (cmd/types.go quitCmd).
#[tokio::test]
async fn quit_commands() {
    let h = Harness::new().await;
    for line in ["q", "quit", "q!", "qa", "Q"] {
        let c = k9rs::cmd::parse(line, &h.ctx.aliases).unwrap();
        assert_eq!(c.gvr, "quit", "{line}");
    }
}

/// `:po kube-system` sets the namespace (cmd interpreter NSArg).
#[tokio::test]
async fn command_with_namespace() {
    let mut h = Harness::new().await;
    h.goto("po kube-system").await.unwrap();
    assert_eq!(h.top().title(), " pods(kube-system)[2] ");
    h.goto("po all").await.unwrap();
    assert_eq!(h.top().title(), " pods(all)[9] ");
    let s = h.render();
    assert!(s.line(1).contains("NAMESPACE"), "{}", s.line(1));
}

/// `:po -l sel` and `:po /filter` (cmd interpreter LabelsArg / FilterArg).
#[tokio::test]
async fn command_label_and_filter_args() {
    let mut h = Harness::new().await;
    h.goto("po -l app=nginx").await.unwrap();
    assert_eq!(h.top().title(), " pods(default)[2] </app=nginx> ");
    // Filters match every field, labels included (model1 rxFilter).
    h.goto("po /web").await.unwrap();
    assert_eq!(h.top().title(), " pods(default)[3] </web> ");
    h.goto("po /web-0").await.unwrap();
    assert_eq!(h.top().title(), " pods(default)[1] </web-0> ");
    assert!(h.render().contains("web-0"));
}
