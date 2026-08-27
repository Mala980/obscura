use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct ContentSecurityPolicy {
    pub directives: HashMap<String, Vec<String>>,
}

impl ContentSecurityPolicy {
    pub fn parse(header: &str) -> Self {
        let mut csp = Self::default();
        for directive in header.split(';') {
            let directive = directive.trim();
            if let Some((name, value)) = directive.split_once(' ') {
                let values: Vec<String> = value.split_whitespace().map(String::from).collect();
                csp.directives.insert(name.to_lowercase(), values);
            }
        }
        csp
    }

    pub fn allows_script_src(&self, url: &str) -> bool {
        self.allows_source("script-src", url)
    }

    pub fn allows_style_src(&self, url: &str) -> bool {
        self.allows_source("style-src", url)
    }

    pub fn allows_connect_src(&self, url: &str) -> bool {
        self.allows_source("connect-src", url)
    }

    fn allows_source(&self, directive: &str, url: &str) -> bool {
        if let Some(sources) = self.directives.get(directive) {
            for source in sources {
                if source == "'self'" || source == "*" { return true; }
                if source == "blob:" && url.starts_with("blob:") { return true; }
                if source == "data:" && url.starts_with("data:") { return true; }
                // Check URL prefix match
                if url.starts_with(source) { return true; }
            }
            return false;
        }
        true // No directive = allows everything
    }

    pub fn report_only(&self) -> bool {
        self.directives.contains_key("report-only")
    }
}