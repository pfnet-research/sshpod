use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSpec {
    pub context: Option<String>,
    pub namespace: Option<String>,
    pub target: Target,
    pub container: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Pod(String),
    Deployment(String),
    Job(String),
}

#[derive(Debug, Error)]
pub enum HostSpecError {
    #[error("hostname must end with .sshpod")]
    MissingSuffix,
    #[error("hostname segment '{segment}' is missing \"--\"")]
    MissingSeparator { segment: String },
    #[error(
        "hostname must include one of pod--/deployment--/job-- (container-- optional, namespace-- optional, context-- optional), ending with .sshpod"
    )]
    InvalidFormat,
}

pub fn parse(host: &str) -> Result<HostSpec, HostSpecError> {
    let trimmed = host.trim_end_matches('.');
    let without_suffix = trimmed
        .strip_suffix(".sshpod")
        .ok_or(HostSpecError::MissingSuffix)?;

    let mut container = None;
    let mut namespace = None;
    let mut context = None;
    let mut target = None;

    for token in without_suffix.split('.').filter(|s| !s.is_empty()) {
        if !token.contains("--") {
            return Err(HostSpecError::MissingSeparator {
                segment: token.to_string(),
            });
        }
        if let Some(rest) = token.strip_prefix("container--") {
            if rest.is_empty() || container.is_some() {
                return Err(HostSpecError::InvalidFormat);
            }
            container = Some(rest.to_string());
            continue;
        }
        if let Some(rest) = token.strip_prefix("namespace--") {
            if rest.is_empty() || namespace.is_some() {
                return Err(HostSpecError::InvalidFormat);
            }
            namespace = Some(rest.to_string());
            continue;
        }
        if let Some(rest) = token.strip_prefix("context--") {
            if rest.is_empty() || context.is_some() {
                return Err(HostSpecError::InvalidFormat);
            }
            context = Some(rest.to_string());
            continue;
        }
        if target.is_none() {
            target = Some(parse_target(token)?);
            continue;
        }
        return Err(HostSpecError::InvalidFormat);
    }

    let target = target.ok_or(HostSpecError::InvalidFormat)?;

    Ok(HostSpec {
        target,
        namespace,
        context,
        container,
    })
}

fn parse_target(token: &str) -> Result<Target, HostSpecError> {
    if token.is_empty() {
        return Err(HostSpecError::InvalidFormat);
    }
    if let Some(rest) = token.strip_prefix("pod--") {
        if rest.is_empty() {
            return Err(HostSpecError::InvalidFormat);
        }
        return Ok(Target::Pod(rest.to_string()));
    }
    if let Some(rest) = token.strip_prefix("deployment--") {
        if rest.is_empty() {
            return Err(HostSpecError::InvalidFormat);
        }
        return Ok(Target::Deployment(rest.to_string()));
    }
    if let Some(rest) = token.strip_prefix("job--") {
        if rest.is_empty() {
            return Err(HostSpecError::InvalidFormat);
        }
        return Ok(Target::Job(rest.to_string()));
    }
    Err(HostSpecError::InvalidFormat)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reject_missing_suffix() {
        let err = parse("pod--app.context--ctx").unwrap_err();
        assert!(matches!(err, HostSpecError::MissingSuffix));
    }

    #[test]
    fn reject_duplicate_tokens() {
        assert!(parse("pod--a.pod--b.context--ctx.sshpod").is_err());
        assert!(parse("namespace--n.namespace--m.pod--a.context--ctx.sshpod").is_err());
        assert!(parse("container--x.container--y.pod--a.context--ctx.sshpod").is_err());
        assert!(parse("context--a.context--b.pod--a.sshpod").is_err());
    }

    #[test]
    fn reject_unknown_prefix() {
        assert!(parse("foo--bar.pod--a.context--ctx.sshpod").is_err());
    }

    #[test]
    fn reject_missing_separator_segment() {
        let err = parse("deployment--ws.context-pfcp-pfn-yh1-01.sshpod").unwrap_err();
        assert!(matches!(
            err,
            HostSpecError::MissingSeparator { ref segment }
                if segment == "context-pfcp-pfn-yh1-01"
        ));
    }

    #[test]
    fn dot_collapse_handling() {
        // Leading empty segment plus empty pod token should still be rejected
        assert!(parse(".pod--.context--ctx.sshpod").is_err());
        // Extra dots are ignored by the parser; this should parse like a single-dot variant
        let spec = parse("pod--app..context--ctx.sshpod").expect("double dots should parse");
        assert_eq!(spec.target, Target::Pod("app".into()));
        assert_eq!(spec.context.as_deref(), Some("ctx"));
    }

    #[test]
    fn round_trip_common_patterns() {
        let cases = [
            ("pod--a.context--c.sshpod", ("a", Some("c"), None, None)),
            (
                "pod--a.namespace--n.context--c.sshpod",
                ("a", Some("c"), Some("n"), None),
            ),
            (
                "deployment--d.namespace--n.context--c.sshpod",
                ("d", Some("c"), Some("n"), None),
            ),
            ("job--j.context--c.sshpod", ("j", Some("c"), None, None)),
            (
                "container--x.pod--a.namespace--n.context--c.sshpod",
                ("a", Some("c"), Some("n"), Some("x")),
            ),
            (
                "pod--app.namespace--ns.sshpod",
                ("app", None, Some("ns"), None),
            ),
        ];
        for (input, (name, ctx, ns, container)) in cases {
            let spec = parse(input).expect("should parse");
            match &spec.target {
                Target::Pod(p) | Target::Deployment(p) | Target::Job(p) => assert_eq!(p, name),
            }
            assert_eq!(spec.context.as_deref(), ctx);
            assert_eq!(spec.namespace.as_deref(), ns);
            assert_eq!(spec.container.as_deref(), container);
        }
    }
}
