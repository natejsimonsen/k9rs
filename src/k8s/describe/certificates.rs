//! kubectl CertificateSigningRequestDescriber (describe.go
//! describeCertificateSigningRequest + certificate.ParseCSR).

use anyhow::{anyhow, Result};
use base64::Engine;
use serde_json::Value;
use x509_parser::extensions::{GeneralName, ParsedExtension};
use x509_parser::prelude::*;

use super::events::describe_events;
use super::util::{format_labels, human_duration, Ctx, J};
use super::writer::{PrefixWriter, L0};

/// pkix.Name fields kubectl prints.
#[derive(Default)]
struct Subject {
    common_name: String,
    serial_number: String,
    organization: Vec<String>,
    organizational_unit: Vec<String>,
    country: Vec<String>,
    locality: Vec<String>,
    province: Vec<String>,
    street_address: Vec<String>,
    postal_code: Vec<String>,
}

#[derive(Default)]
struct Csr {
    subject: Subject,
    dns_names: Vec<String>,
    emails: Vec<String>,
    uris: Vec<String>,
    ips: Vec<String>,
}

/// certificate.ParseCSR: PEM `CERTIFICATE REQUEST` then PKCS#10 DER.
fn parse_csr(pem_bytes: &[u8]) -> Result<Csr> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(pem_bytes)
        .map_err(|_| anyhow!("PEM block type must be CERTIFICATE REQUEST"))?;
    if pem.label != "CERTIFICATE REQUEST" {
        return Err(anyhow!("PEM block type must be CERTIFICATE REQUEST"));
    }
    let (_, req) =
        X509CertificationRequest::from_der(&pem.contents).map_err(|e| anyhow!("asn1: {e}"))?;
    let mut csr = Csr::default();
    // pkix.Name.FillFromRDNSequence: the last CN/serial wins, lists append.
    for rdn in req.certification_request_info.subject.iter() {
        for atv in rdn.iter() {
            let Ok(v) = atv.as_str() else { continue };
            let v = v.to_string();
            let s = &mut csr.subject;
            match atv.attr_type().to_id_string().as_str() {
                "2.5.4.3" => s.common_name = v,
                "2.5.4.5" => s.serial_number = v,
                "2.5.4.6" => s.country.push(v),
                "2.5.4.7" => s.locality.push(v),
                "2.5.4.8" => s.province.push(v),
                "2.5.4.9" => s.street_address.push(v),
                "2.5.4.10" => s.organization.push(v),
                "2.5.4.11" => s.organizational_unit.push(v),
                "2.5.4.17" => s.postal_code.push(v),
                _ => {}
            }
        }
    }
    if let Some(exts) = req.requested_extensions() {
        for e in exts {
            if let ParsedExtension::SubjectAlternativeName(san) = e {
                for n in &san.general_names {
                    match n {
                        GeneralName::DNSName(d) => csr.dns_names.push(d.to_string()),
                        GeneralName::RFC822Name(m) => csr.emails.push(m.to_string()),
                        GeneralName::URI(u) => csr.uris.push(u.to_string()),
                        GeneralName::IPAddress(b) => csr.ips.push(ip_string(b)),
                        _ => {}
                    }
                }
            }
        }
    }
    Ok(csr)
}

/// net.IP.String.
fn ip_string(b: &[u8]) -> String {
    match b.len() {
        4 => std::net::Ipv4Addr::new(b[0], b[1], b[2], b[3]).to_string(),
        16 => {
            let mut a = [0u8; 16];
            a.copy_from_slice(b);
            let v6 = std::net::Ipv6Addr::from(a);
            match v6.to_ipv4_mapped() {
                Some(v4) => v4.to_string(),
                None => v6.to_string(),
            }
        }
        _ => {
            let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
            format!("?{hex}")
        }
    }
}

/// extractCSRStatus.
fn csr_status(csr: J) -> String {
    let (mut approved, mut denied, mut failed) = (false, false, false);
    for c in csr.arr("status.conditions") {
        match c.s("type") {
            "Approved" => approved = true,
            "Denied" => denied = true,
            "Failed" => failed = true,
            _ => {}
        }
    }
    let mut status = if denied {
        "Denied"
    } else if approved {
        "Approved"
    } else {
        "Pending"
    }
    .to_string();
    if failed {
        status.push_str(",Failed");
    }
    if !csr.s("status.certificate").is_empty() {
        status.push_str(",Issued");
    }
    status
}

/// printListHelper.
fn print_list(w: &mut PrefixWriter, prefix: &str, name: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    let sep = format!("\n{prefix}\t");
    w.write(L0, &format!("{prefix}{name}:\t{}\n", values.join(&sep)));
}

pub(crate) fn describe_csr(ctx: &Ctx, csr: J, events: &[Value]) -> Result<String> {
    let request = base64::engine::general_purpose::STANDARD
        .decode(csr.s("spec.request"))
        .unwrap_or_default();
    let cr = parse_csr(&request).map_err(|e| anyhow!("Error parsing CSR: {e}"))?;
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", csr.s("metadata.name")));
    w.write(
        L0,
        &format!("Labels:\t{}\n", format_labels(&csr.smap("metadata.labels"))),
    );
    w.write(
        L0,
        &format!(
            "Annotations:\t{}\n",
            format_labels(&csr.smap("metadata.annotations"))
        ),
    );
    w.write(
        L0,
        &format!(
            "CreationTimestamp:\t{}\n",
            ctx.rfc1123z(csr.get("metadata.creationTimestamp"))
        ),
    );
    w.write(
        L0,
        &format!("Requesting User:\t{}\n", csr.s("spec.username")),
    );
    let signer = csr.s("spec.signerName");
    if !signer.is_empty() {
        w.write(L0, &format!("Signer:\t{signer}\n"));
    }
    if let Some(secs) = csr.opt_i("spec.expirationSeconds") {
        w.write(
            L0,
            &format!(
                "Requested Duration:\t{}\n",
                human_duration(secs.saturating_mul(1_000_000_000))
            ),
        );
    }
    w.write(L0, &format!("Status:\t{}\n", csr_status(csr)));
    let s = &cr.subject;
    w.write(L0, "Subject:\n");
    w.write(L0, &format!("\tCommon Name:\t{}\n", s.common_name));
    w.write(L0, &format!("\tSerial Number:\t{}\n", s.serial_number));
    print_list(&mut w, "\t", "Organization", &s.organization);
    print_list(&mut w, "\t", "Organizational Unit", &s.organizational_unit);
    print_list(&mut w, "\t", "Country", &s.country);
    print_list(&mut w, "\t", "Locality", &s.locality);
    print_list(&mut w, "\t", "Province", &s.province);
    print_list(&mut w, "\t", "StreetAddress", &s.street_address);
    print_list(&mut w, "\t", "PostalCode", &s.postal_code);
    if cr.dns_names.len() + cr.emails.len() + cr.ips.len() + cr.uris.len() > 0 {
        w.write(L0, "Subject Alternative Names:\n");
        print_list(&mut w, "\t", "DNS Names", &cr.dns_names);
        print_list(&mut w, "\t", "Email Addresses", &cr.emails);
        print_list(&mut w, "\t", "URIs", &cr.uris);
        print_list(&mut w, "\t", "IP Addresses", &cr.ips);
    }
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}
