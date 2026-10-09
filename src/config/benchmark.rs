//! benchmarks.yaml (config/benchmark.go): per context `hey` benchmark specs.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::yaml::{de_bool, de_null};

/// Default concurrency.
pub const DEFAULT_C: i32 = 1;
/// Default number of requests.
pub const DEFAULT_N: i32 = 200;
/// Default HTTP verb.
pub const DEFAULT_METHOD: &str = "GET";

/// Root of benchmarks.yaml (config.Bench).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bench {
    pub benchmarks: Benchmarks,
}

impl Default for Bench {
    fn default() -> Self {
        Bench {
            benchmarks: Benchmarks {
                defaults: Benchmark {
                    concurrency: DEFAULT_C,
                    requests: DEFAULT_N,
                },
                services: BTreeMap::new(),
                containers: BTreeMap::new(),
            },
        }
    }
}

/// config.Benchmarks.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Benchmarks {
    #[serde(deserialize_with = "de_null")]
    pub defaults: Benchmark,
    #[serde(deserialize_with = "de_null")]
    pub services: BTreeMap<String, BenchConfig>,
    #[serde(deserialize_with = "de_null")]
    pub containers: BTreeMap<String, BenchConfig>,
}

/// A generic benchmark (config.Benchmark).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Benchmark {
    pub concurrency: i32,
    pub requests: i32,
}

impl Benchmark {
    /// Whether the benchmark is unset.
    pub fn empty(&self) -> bool {
        self.concurrency == 0 && self.requests == 0
    }
}

/// Basic auth credentials.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Auth {
    #[serde(deserialize_with = "de_null")]
    pub user: String,
    #[serde(deserialize_with = "de_null")]
    pub password: String,
}

/// HTTP request spec.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Http {
    #[serde(deserialize_with = "de_null")]
    pub method: String,
    #[serde(deserialize_with = "de_null")]
    pub host: String,
    #[serde(deserialize_with = "de_null")]
    pub path: String,
    #[serde(deserialize_with = "de_bool")]
    pub http2: bool,
    #[serde(deserialize_with = "de_null")]
    pub body: String,
    #[serde(deserialize_with = "de_null")]
    pub headers: BTreeMap<String, Vec<String>>,
}

/// A service/container benchmark (config.BenchConfig).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BenchConfig {
    #[serde(deserialize_with = "de_null")]
    pub name: String,
    pub concurrency: i32,
    pub requests: i32,
    #[serde(deserialize_with = "de_null")]
    pub auth: Auth,
    #[serde(deserialize_with = "de_null")]
    pub http: Http,
}

/// Default bench spec (config.DefaultBenchSpec).
pub fn default_bench_spec() -> BenchConfig {
    BenchConfig {
        concurrency: DEFAULT_C,
        requests: DEFAULT_N,
        http: Http {
            method: DEFAULT_METHOD.into(),
            path: "/".into(),
            ..Http::default()
        },
        ..BenchConfig::default()
    }
}

impl Bench {
    /// config.NewBench: defaults overlaid with `path`.
    pub fn load(path: &Path) -> Result<Bench> {
        let mut b = Bench::default();
        b.reload(path)?;
        Ok(b)
    }

    /// Re-read `path` over the current values.
    pub fn reload(&mut self, path: &Path) -> Result<()> {
        let bb = std::fs::read_to_string(path)?;
        let over = super::yaml::parse_value(&bb)?;
        let mut base = serde_yaml::to_value(&*self)?;
        super::yaml::merge_value(&mut base, over);
        *self = serde_yaml::from_value(base)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn td(p: &str) -> std::path::PathBuf {
        format!(
            "{}/src/config/testdata/config/benchmarks/{p}",
            env!("CARGO_MANIFEST_DIR")
        )
        .into()
    }

    #[test]
    fn empty() {
        assert!(Benchmark::default().empty());
        assert!(!Bench::default().benchmarks.defaults.empty());
    }

    #[test]
    fn load() {
        for (f, c, n, svc, co) in [
            ("b_good.yaml", 2, 1000, 2, 0),
            ("b_toast.yaml", 1, 200, 0, 0),
        ] {
            let b = Bench::load(&td(f)).unwrap();
            assert_eq!(b.benchmarks.defaults.concurrency, c, "{f}");
            assert_eq!(b.benchmarks.defaults.requests, n, "{f}");
            assert_eq!(b.benchmarks.services.len(), svc, "{f}");
            assert_eq!(b.benchmarks.containers.len(), co, "{f}");
        }
        assert!(Bench::load(&td("toast.yaml")).is_err());
    }

    #[test]
    fn services() {
        let b = Bench::load(&td("b_good.yaml")).unwrap();
        let headers: BTreeMap<String, Vec<String>> = [
            ("Accept".to_string(), vec!["text/html".to_string()]),
            (
                "Content-Type".to_string(),
                vec!["application/json".to_string()],
            ),
        ]
        .into_iter()
        .collect();
        for (k, c, n, m, h, p, h2) in [
            ("default/nginx", 2, 1000, "GET", "10.10.10.10", "/", true),
            ("blee/fred", 10, 1500, "POST", "20.20.20.20", "/zorg", false),
        ] {
            let s = &b.benchmarks.services[k];
            assert_eq!(s.concurrency, c);
            assert_eq!(s.requests, n);
            assert_eq!(s.http.method, m);
            assert_eq!(s.http.host, h);
            assert_eq!(s.http.path, p);
            assert_eq!(s.http.http2, h2);
            assert_eq!(s.http.body, r#"{"fred": "blee"}"#);
            assert_eq!(
                s.auth,
                Auth {
                    user: "fred".into(),
                    password: "blee".into()
                }
            );
            assert_eq!(s.http.headers, headers);
        }
    }

    #[test]
    fn reload() {
        let mut b = Bench::load(&td("b_containers.yaml")).unwrap();
        assert_eq!(b.benchmarks.defaults.concurrency, 2);
        b.reload(&td("b_containers_1.yaml")).unwrap();
        assert_eq!(b.benchmarks.defaults.concurrency, 20);
        assert_eq!(default_bench_spec().http.method, "GET");
    }
}
