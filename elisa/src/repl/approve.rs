use super::*;

impl<'f> Session<'f> {

    /// Whether this session's requests leave the machine: a cloud connection, or no connection and a non-loopback
    /// `base_url` (then `redact = "remote"` hides secrets from the model).
    pub(super) fn model_leaves(&self) -> bool {
        self.cfg.leaves.is_some() || (self.cfg.provider.is_empty() && !crate::provider::is_local(&self.cfg.base_url))
    }

    /// The `<untrusted>` fence source for a result of tool `name`, None when it is trusted (`untrusted::source`).
    pub(super) fn untrusted_source(&self, name: &str, input: &serde_json::Value) -> Option<String> {
        let server = self.mcp.server_of(name);
        // a plugin's server is third-party code like a configured one
        let user = server.is_some_and(|s| self.cfg.mcp.iter().any(|c| c.name == s) || self.plugin_servers.iter().any(|(_, n)| n == s));
        crate::untrusted::source(name, server, user, input)
    }

    /// Tool output on its way to the model: secrets hidden (config `redact`, `secrets.rs`) and outside text fenced
    /// (`untrusted.rs`). The user's own display (`tool_done`) shows the output as it came.
    pub(super) fn guard_output(&mut self, name: &str, input: &serde_json::Value, content: String) -> String {
        // whatever the mode: a secret seen here is hidden wherever it shows up again on disk (`secrets::Scrubber`)
        crate::secrets::learn(&content);
        let mode = crate::secrets::Mode::parse(&self.cfg.redact).unwrap_or(crate::secrets::Mode::Remote);
        let content = if mode.applies(self.model_leaves()) {
            let (r, n) = crate::secrets::redact(&content);
            if n > 0 { self.fe.info(&format!("[{} hidden from the model]", crate::secrets::count_words(n))); }
            r
        } else { content };
        // `read` of an outside document (a PDF, a saved web page …) is fenced too (`untrusted::read_source`)
        let doc = || if name == "read" { crate::untrusted::read_source(input, &content) } else { None };
        let Some(src) = self.untrusted_source(name, input).or_else(doc) else { return content };
        let (fenced, hit) = crate::untrusted::fence(&src, &content);
        if let Some(h) = hit {
            self.fe.info(&format!("[{name}: text that looks like a prompt injection (\"{h}\"); marked for the model as data]"));
        }
        fenced
    }

    /// May this tool call run? A call that reaches outside the project folder goes through `approve_outside` first
    /// (its own rules: never the per-project always-allow list, never `--yes` for writes).
    pub(super) fn approve(&mut self, name: &str, input: &serde_json::Value, summary: &str) -> Result<(), String> {
        // a file-tool call that cannot change anything (URL path, old == new, old not found, binary file) fails
        // before anyone is asked (`tools::precheck`); MCP tools are their servers' business
        if !self.mcp.owns(name) {
            if let Err(e) = tools::precheck(name, input, &self.tctx) { self.answer = Some("invalid"); return Err(e); }
        }
        // pattern rules (`[permissions]`, plugins; `perms.rs`): a deny stops the call here, an ask always asks, an
        // allow skips the prompt (never for a server whose tool list changed)
        let server = self.mcp.server_of(name).map(String::from);
        let remote = self.mcp.remote_of(name).map(String::from);
        let rule = self.cfg.perms.decide(name, input, server.as_deref(), remote.as_deref(), &self.cwd)
            .map(|r| RuleHit { level: r.level, pattern: r.pattern.clone(), source: r.source.clone() });
        if let Some(r) = rule.as_ref().filter(|r| r.level == Level::Deny) {
            self.answer = Some("rule");
            return Err(format!("denied by the rule deny {} ({}): {NO_RETRY}", r.pattern, r.source));
        }
        if let Some(o) = outside::check(name, input, &self.cwd) { return self.approve_outside(name, summary, &o, input, rule); }
        self.approve_inside(name, summary, input, rule)
    }

    /// Outside the project (config `outside`): deny → denied; allow → the normal rules; read-ok → reads take the
    /// normal rules, writes ask; ask → ask (Yes once / Always = these paths for this session / No). Writes outside
    /// (and bash naming outside paths) under `--yes` / approve-all are denied unless `outside = "allow"`; with nobody
    /// to ask (-p without a terminal) the call is denied. Paths granted for the session pass without asking.
    fn approve_outside(&mut self, name: &str, summary: &str, o: &Outside, input: &serde_json::Value, rule: Option<RuleHit>) -> Result<(), String> {
        let list = o.paths.join(", ");
        let what = o.access_word();
        match self.outside_mode {
            outside::Mode::Deny => return Err(format!("denied: {what} outside the project folder ({list}); config outside = \"deny\"")),
            outside::Mode::Allow => return self.approve_inside(name, summary, input, rule),
            outside::Mode::ReadOk if !o.write() => return self.approve_inside(name, summary, input, rule),
            _ => {}
        }
        if outside::covered(&self.outside_ok, o) { return Ok(()); }
        if self.yolo && o.write() {
            return Err(format!("denied: {what} outside the project folder ({list}) is never auto-approved by --yes / approve-all; \
                set outside = \"allow\" in ~/.config/elisa/config.toml to allow it"));
        }
        if !self.interactive {
            return Err(format!("denied: {what} outside the project folder ({list}) needs a person to approve it (non-interactive){}",
                if o.write() { "" } else { "; outside = \"read-ok\" allows outside reads" }));
        }
        self.acts.waiting_ok(&mut *self.fe, true);
        let a = self.fe.approve_outside(name, summary, o);
        if a != Approval::No { self.acts.waiting_ok(&mut *self.fe, false); }
        self.answer = Some(answer_word(&a));
        if CANCEL.load(Ordering::SeqCst) { self.answer = Some("cancelled"); return Err("cancelled by user".into()); }
        match a {
            Approval::Yes => Ok(()),
            Approval::Always => {
                for p in &o.paths {
                    let p = std::path::Path::new(p);
                    if p.is_absolute() { self.outside_ok.push((p.to_path_buf(), o.write())); }
                }
                self.fe.info(&format!("allowed for this session ({}): {list}", if o.write() { "read + write" } else { "read" }));
                Ok(())
            }
            Approval::Rule => Ok(()),
            Approval::No => Err(format!("denied by user ({what} outside the project folder: {list}): {SAID_NO}")),
        }
    }

    /// one audit line about a tool call: its tool, its MCP server (if any) and its summary, plus `fields`
    pub(super) fn audit_tool(&self, event: &str, name: &str, summary: &str, fields: &[(&str, &str)]) {
        if !self.audit.on() { return; }
        let server = self.mcp.server_of(name).unwrap_or("").to_string();
        let mut f: Vec<(&str, &str)> = vec![("tool", name), ("server", &server), ("summary", summary)];
        f.extend_from_slice(fields);
        self.audit.note(event, &f);
    }

    /// `/audit [N]`: the last N (20) entries of this project's audit log, and whether its hash chain holds
    pub(super) fn audit_command(&mut self, arg: &str) {
        let Some(path) = self.audit.path().map(std::path::Path::to_path_buf) else { self.fe.info("the audit log is off (config audit = \"off\")"); return };
        if !path.exists() { self.fe.info(&format!("audit log empty ({})", path.display())); return; }
        let (n, note) = match crate::audit::count_arg(arg) { Ok(x) => x, Err(u) => { self.fe.error(&u); return } };
        let lines = crate::audit::last(&path, n);
        if let Some(t) = note { self.fe.info(&t); }
        if !lines.is_empty() { self.fe.info(&lines.join("\n")); }
        match crate::audit::verify(&path) {
            Ok(k) => self.fe.info(&format!("chain ok ({k} entries) · {}", path.display())),
            Err((l, why)) => self.fe.error(&format!("chain broken at line {l}: {why} · {}", path.display())),
        }
    }

        fn approve_inside(&mut self, name: &str, summary: &str, input: &serde_json::Value, rule: Option<RuleHit>) -> Result<(), String> {
        // outline / read_symbol go with `read`, edit_symbol with `edit`; an MCP tool goes with its server (`web`)
        // MCP keys live in their own namespace (`mcp:web`), so a server named like one of Elisa's tools can never
        // share its always-allow / auto_approve entry; auto_approve matches MCP tools only by `mcp:SERVER`
        // the built-in reasoning tools (think, steps, step_done, calc) have no side effects: never asked
        if self.mcp.reason_tool(name) { return Ok(()); }
        let server = self.mcp.server_of(name).map(String::from);
        let key = match &server { Some(s) => tools::mcp_key(s), None => tools::approval_name(name).to_string() };
        let key = key.as_str();
        // a server whose tool list changed since it was pinned (`pins.rs`): no always-allow, no --yes, until /trust
        let untrusted = server.as_deref().is_some_and(|s| self.mcp.untrusted(s));
        if untrusted && !self.interactive {
            let s = server.as_deref().unwrap_or("");
            return Err(format!("denied: MCP server {s}'s tool list changed since it was pinned, so each call needs a person to approve it; \
                check the server, then /trust {s} in an interactive session"));
        }
        // an ask rule: a person decides each time (no --yes, no always-allow, no allow rule gets past it)
        let ask = rule.as_ref().filter(|r| r.level == Level::Ask);
        if let Some(r) = ask {
            if !self.interactive {
                return Err(format!("denied: the rule ask {} ({}) needs a person to approve each call (non-interactive)", r.pattern, r.source));
            }
        } else {
            if !untrusted && rule.as_ref().is_some_and(|r| r.level == Level::Allow) { self.answer = Some("rule"); return Ok(()); }
            // the built-in documents server reading a document the person attached this session: they gave it to read
            if !untrusted && server.as_deref() == Some("docs") && self.reads_upload(input) { return Ok(()); }
            if !untrusted && (self.yolo || self.always.contains(key) || self.cfg.auto_approve.iter().any(|a| a == key || (server.is_none() && a == name))) { return Ok(()); }
            if !self.interactive {
                // nobody to ask: local reads run; MCP tools (the web too) are denied like a write
                return if tools::access(name) == Access::ReadOnly && server.is_none() { Ok(()) } else { Err("denied (non-interactive; use --yes)".into()) };
            }
        }
        let summary = match ask { Some(r) => format!("{summary}\nasked by the rule ask {} ({})", r.pattern, r.source), None => summary.to_string() };
        // "always allow this pattern": offered when nothing forces a prompt and there is a narrow pattern for the call
        let offer = if ask.is_none() && !untrusted && self.user_config().is_some() {
            crate::perms::suggest(name, input, server.as_deref(), self.mcp.remote_of(name), &self.cwd)
        } else { None };
        self.acts.waiting_ok(&mut *self.fe, true);
        let a = match &offer {
            Some(p) => self.fe.approve_rule(name, &summary, server.as_deref(), p),
            None => self.fe.approve(name, &summary, server.as_deref()),
        };
        if a != Approval::No { self.acts.waiting_ok(&mut *self.fe, false); }
        self.answer = Some(answer_word(&a));
        if CANCEL.load(Ordering::SeqCst) { self.answer = Some("cancelled"); return Err("cancelled by user".into()); }
        match a {
            Approval::Yes => Ok(()),
            Approval::Always | Approval::Rule if ask.is_some() => {
                self.fe.info("allowed once: an ask rule asks every time");
                Ok(())
            }
            Approval::Rule => {
                let (Some(p), Some(path)) = (offer, self.user_config()) else { return Ok(()) };
                match crate::perms::save_allow(&path, &p) {
                    Ok(()) => {
                        let _ = self.cfg.perms.add(Level::Allow, &p, &path.display().to_string());
                        self.audit.note("rule", &[("summary", &format!("allow {p}")), ("outcome", "saved"), ("detail", &path.display().to_string())]);
                        self.fe.info(&format!("always allowed: {p} (saved to {})", path.display()));
                    }
                    Err(e) => self.fe.error(&format!("the rule {p} was not saved: {e}")),
                }
                Ok(())
            }
            Approval::Always if untrusted => {
                self.fe.info(&format!("allowed once: \"always\" stays off for MCP server {} until /trust {0}", server.as_deref().unwrap_or("")));
                Ok(())
            }
            Approval::Always => {
                // kept per project (.elisa/allow), so a restart does not ask again
                self.always.insert(key.to_string());
                if let Err(e) = self.store.save_allowed(&self.always) { self.fe.error(&format!("could not save always-allow for {name}: {e}")); }
                Ok(())
            }
            Approval::No => Err(format!("denied by user: {SAID_NO}")),
        }
    }
}

/// an approval answer as the audit log writes it
fn answer_word(a: &Approval) -> &'static str {
    match a { Approval::Yes => "yes", Approval::Always => "always", Approval::Rule => "rule", Approval::No => "no" }
}

/// the permission rule that decided a call (`perms::Rules::decide`)
#[derive(Clone)]
struct RuleHit { level: Level, pattern: String, source: String }

/// added to a denial: nothing happened, say so, and do not ask again for the same thing (measured on qwen3-vl:2b:
/// with only "do not call this tool again…; answer without it" a denied write got "I've saved "hello" to deny.txt")
const NO_RETRY: &str = "NOTHING was done: no file was written or changed and no command ran. Tell the user plainly that it was \
    not done and why; never say it was saved or done. Do not call this tool again for this request";
/// a call the user said no to: nothing happened, and a sample sentence (the 2B model, told only "say why", said
/// "I don't have the capability to write files")
const SAID_NO: &str = "the user said no, so NOTHING was done: no file was written or changed and no command ran. Reply in one \
    short sentence like \"Okay, I didn't do that, since you said no.\"; never say it was saved or done. Do not call this tool again for this request";
