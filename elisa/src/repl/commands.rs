use super::*;
use super::memory::texts;
use super::startup::help_text;

impl<'f> Session<'f> {

    /// `/model`: the connection in use and the saved ones. `/model NAME`: the active connection set to that
    /// installed model (`provider::use_model`); the app restarts its session on it, a terminal session on its next start.
    fn model_command(&mut self, name: &str) {
        use crate::provider::{path_in, summary, Providers};
        if !name.is_empty() {
            let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return };
            match crate::provider::use_model(&home, name, &|k| std::env::var(k).ok()) {
                Ok((c, m, false)) => self.fe.info(&format!("connection {c} already uses {m}")),
                Ok((c, m, true)) => self.fe.info(&format!("connection {c} now uses {m}: it takes effect when Elisa starts again (elisa --continue-open keeps this conversation)")),
                Err((e, installed)) => {
                    self.fe.error(&e);
                    if !installed.is_empty() { self.fe.info(&format!("installed: {}", installed.join(", "))); }
                }
            }
            return;
        }
        self.fe.info(&format!("model {} @ {} · connection {}", self.cfg.model, self.cfg.base_url, if self.cfg.provider.is_empty() { "none (config.toml)" } else { &self.cfg.provider }));
        let mut caps = self.client.caps().clone();
        if self.client.text_tools() { caps.tools = crate::backend::Tools::Text; }
        for l in crate::backend::lines(&caps, self.max_ctx) { self.fe.info(&l); }
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return };
        match Providers::load(&home) {
            Ok(f) => for l in summary(&self.cfg.provider, &f, &path_in(&home)) { self.fe.info(&l); },
            Err(e) => self.fe.error(&e),
        }
    }

    /// Estimated tokens of this request's tool definitions by owner: Elisa's own, then each MCP server whose tools
    /// are offered this turn; largest first.
    pub(super) fn tool_costs(&self) -> Vec<(String, usize)> {
        let own: Vec<&ToolDef> = self.tools.iter().filter(|t| !self.mcp.owns(&t.name)).collect();
        let mut parts: Vec<(String, usize)> = self.mcp.costs().into_iter()
            .filter(|(srv, _)| self.tools.iter().any(|t| self.mcp.server_of(&t.name) == Some(srv.as_str()))).collect();
        parts.push(("Elisa's own tools".into(), estimate_tokens(&serde_json::to_string(&own).unwrap_or_default())));
        parts.sort_by_key(|p| std::cmp::Reverse(p.1));
        parts
    }

    /// The MCP servers that do not run because config switched them off (for `/mcp`): user servers with
    /// `enabled = false`, and Elisa's own servers with the setting that keeps each out.
    fn mcp_off(&self) -> Vec<crate::mcp_host::Off> {
        use crate::mcp_host::Off;
        let c = &self.cfg;
        let off_by = |n: &str| c.mcp_off.iter().any(|o| o == n);
        let mut v: Vec<Off> = Vec::new();
        let builtins: [(&str, bool, String); 5] = [
            ("web", c.web.is_empty() || c.web == "off", "web = \"off\"".into()),
            ("docs", c.docs != "on", "docs = \"off\"".into()),
            ("sec", c.security != "on", "security = \"off\"".into()),
            ("run", c.sandbox != "colima", format!("sandbox = \"{}\"", c.sandbox)),
            (crate::mcp_reason::NAME, c.reason == "off", "reason = \"off\"".into()),
        ];
        for (name, setting_off, why) in builtins.iter().cloned() {
            if self.mcp.running(name) || c.mcp.iter().any(|s| s.name == name) { continue; }
            if off_by(name) { v.push(Off { name: name.into(), builtin: true, why: format!("[mcp.{name}] enabled = false") }); }
            else if setting_off { v.push(Off { name: name.into(), builtin: true, why }); }
        }
        for n in c.mcp_off.iter().filter(|n| !builtins.iter().any(|b| b.0 == n.as_str())) {
            v.push(Off { name: n.clone(), builtin: false, why: format!("[mcp.{n}] enabled = false") });
        }
        v
    }

    /// `/mcp [NAME]`: the MCP servers, or one of them with its tools' descriptions (`mcp_host::Host::report`)
    fn mcp_command(&mut self, name: &str) {
        let off = self.mcp_off();
        let (gate, reason_on) = (self.gate, self.reason_on);
        let off_turn = |srv: &str| match srv { "docs" => !gate.0, "sec" => !gate.1, n if n == crate::mcp_reason::NAME => !reason_on, _ => false };
        if name.is_empty() { return self.fe.info(&self.mcp.report(&off, &off_turn)); }
        match self.mcp.report_one(name, &off, &off_turn) {
            Ok(t) => self.fe.info(&t),
            Err(e) => self.fe.error(&e),
        }
    }

    // ----- plugins, file commands, pattern rules (plugins.rs, commands.rs, perms.rs) -----

    /// the user's config file, where "always allow this pattern" saves its rule
    pub(super) fn user_config(&self) -> Option<PathBuf> { self.home.as_ref().map(|h| h.join(".config/elisa/config.toml")) }

    /// Startup: the user's command files, and the project's commands and plugins — loaded when the user trusted this
    /// exact content before, else kept back until the trust prompt (asked lazily: the first time one of its commands
    /// runs, or on `/plugin trust project`).
    pub(super) fn load_commands(&mut self) {
        let mut notes = Vec::new();
        if let Some(h) = &self.home { self.cmd_user = crate::commands::load_dir(&h.join(".elisa/commands"), &crate::commands::Source::User, &mut notes); }
        if let Some(p) = plugins::load_project(&self.cwd) {
            let trusted = self.home.as_ref().is_some_and(|h| plugins::project_trusted(h, &self.cwd, &p.hash));
            if trusted { self.apply_project(p); } else {
                self.fe.info(&format!("this project has its own commands or rules ({}); they load once you trust them: run one, or /plugin trust project",
                    project_summary(&p)));
                self.proj_infos = p.infos.clone();
                self.proj_pending = Some(p);
            }
        }
        for n in notes { self.fe.info(&n); }
        let notes = self.merge_commands();
        for n in notes { self.fe.info(&n); }
    }

    /// the file commands merged again (Elisa's own commands win a clash); the notes say what was left out
    fn merge_commands(&mut self) -> Vec<String> {
        let mut notes = Vec::new();
        let lists = vec![self.cmd_user.clone(), self.cmd_plugins.clone(), self.cmd_project.clone()];
        self.file_cmds = crate::commands::merge(lists, &builtin_command, &mut notes);
        notes
    }

    /// a trusted project's commands and deny / ask rules, in effect from now on
    fn apply_project(&mut self, p: plugins::Project) {
        for (lv, pat, src) in &p.rules {
            if let Err(e) = self.cfg.perms.add(*lv, pat, src) { self.fe.info(&format!("{src}: rule {pat:?} skipped: {e}")); }
        }
        for n in &p.notes { self.fe.info(n); }
        self.cmd_project = p.commands;
        self.proj_infos = p.infos.into_iter().map(|mut i| { i.trusted = true; i }).collect();
    }

    /// The one-time trust prompt for the project's commands and plugins: their full text, then yes / no. True: loaded.
    fn trust_project(&mut self) -> bool {
        let Some(p) = self.proj_pending.take() else { self.fe.info("this project has no commands or plugins waiting for trust"); return false };
        let Some(home) = self.home.clone() else { self.fe.error("no home folder: the trust cannot be kept"); self.proj_pending = Some(p); return false };
        if !self.interactive { self.fe.error("trusting a project's commands needs a person to say yes (interactive session)"); self.proj_pending = Some(p); return false; }
        self.fe.info(&p.text);
        self.fe.event("project_trust", &json!({"path": self.cwd.display().to_string(), "hash": p.hash, "text": p.text,
            "commands": p.commands.iter().map(|c| json!({"name": c.name, "description": c.description, "source": c.source.kind()})).collect::<Vec<_>>(),
            "rules": p.rules.iter().map(|(l, pat, src)| json!({"level": l.word(), "pattern": pat, "source": src})).collect::<Vec<_>>(),
            "plugins": p.infos.iter().map(|i| i.name.clone()).collect::<Vec<_>>()}));
        let yes = self.fe.ask_yes_no("project commands", "Trust this project's commands and rules? (kept in ~/.elisa; any change asks again)");
        let path = self.cwd.display().to_string();
        if !yes {
            self.audit.note("plugin", &[("summary", "trust project"), ("outcome", "declined"), ("detail", &path)]);
            self.fe.info("not trusted: the project's commands stay off (/plugin trust project to review again)");
            self.proj_declined = true;
            self.proj_pending = Some(p);
            return false;
        }
        if let Err(e) = plugins::trust_project(&home, &self.cwd, &p.hash) { self.fe.error(&format!("the trust was not saved: {e}")); }
        self.audit.note("plugin", &[("summary", "trust project"), ("outcome", "ok"), ("detail", &path)]);
        self.proj_declined = false;
        self.apply_project(p);
        for n in self.merge_commands() { self.fe.info(&n); }
        self.fe.info("project commands and rules loaded");
        self.plugins_event();
        true
    }

    /// A line that names a file command: the turn it becomes (Some(None): handled here, nothing to send — a declined
    /// trust prompt). None: not a file command.
    pub(super) fn expand_command(&mut self, line: &str) -> Option<Option<String>> {
        let (cmd, args) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        if builtin_command(cmd) { return None; }
        // its file changed since it was read: read again (a project's or plugin's needs its trust again first)
        if let Some(src) = self.file_cmds.iter().find(|c| c.name == cmd && c.changed_on_disk()).map(|c| c.source.clone()) {
            if !self.reread_commands(cmd, &src) { return Some(None); }
            return match self.file_cmds.iter().find(|c| c.name == cmd) {
                Some(c) => Some(Some(c.prompt(args))),
                None => { self.fe.error(&format!("{cmd}: its file is gone or no longer a valid command (/help lists the commands)")); Some(None) }
            };
        }
        if let Some(c) = self.file_cmds.iter().find(|c| c.name == cmd) { return Some(Some(c.prompt(args))); }
        // a project command not trusted yet: the trust prompt first
        if self.proj_pending.as_ref().is_some_and(|p| p.commands.iter().any(|c| c.name == cmd)) {
            if self.proj_declined {
                self.fe.error(&format!("{cmd} is a project command you have not trusted: /plugin trust project to review it"));
                return Some(None);
            }
            if !self.trust_project() { return Some(None); }
            return Some(self.file_cmds.iter().find(|c| c.name == cmd).map(|c| c.prompt(args)));
        }
        None
    }

    /// A command file changed on disk since it was loaded: its source read again. The user's own: the new text is used.
    /// A plugin's: loaded again only while the plugin's files still match what the user approved (else it is off
    /// until /trust). A project's: the trust prompt again, with the new text, when the content no longer matches the
    /// trusted hash. False: nothing to run (declined, or off).
    fn reread_commands(&mut self, cmd: &str, src: &crate::commands::Source) -> bool {
        use crate::commands::Source;
        match src {
            Source::User => {
                let mut notes = Vec::new();
                if let Some(h) = &self.home { self.cmd_user = crate::commands::load_dir(&h.join(".elisa/commands"), &Source::User, &mut notes); }
                for n in notes.into_iter().chain(self.merge_commands()) { self.fe.info(&n); }
                self.fe.info(&format!("{cmd}: its file changed since it was loaded; using the new text"));
                true
            }
            Source::Plugin(p) => {
                let p = p.clone();
                self.reload_plugins(None);
                if self.plugin_infos.iter().any(|i| i.name == p && i.enabled && i.trusted) {
                    self.fe.info(&format!("{cmd}: its file changed since it was loaded; using the new text"));
                    return true;
                }
                self.fe.error(&format!("{cmd}: plugin {p}'s files changed since you approved them, so its commands are off and nothing ran — review with /plugin list, then /trust {p}"));
                self.plugins_event();
                false
            }
            Source::Project | Source::ProjectPlugin(_) => {
                self.cfg.perms.rules.retain(|r| !r.source.starts_with("project plugin"));
                self.cmd_project.clear();
                let Some(p) = plugins::load_project(&self.cwd) else {
                    for n in self.merge_commands() { self.fe.info(&n); }
                    return true;
                };
                if self.home.as_ref().is_some_and(|h| plugins::project_trusted(h, &self.cwd, &p.hash)) {
                    self.apply_project(p);
                    for n in self.merge_commands() { self.fe.info(&n); }
                    return true;
                }
                // never the old cached text, never the new text unseen: the same trust prompt as at the start
                self.fe.info(&format!("{cmd}: the project's commands changed on disk since you trusted them; review the new text"));
                self.proj_infos = p.infos.clone();
                self.proj_pending = Some(p);
                self.proj_declined = false;
                for n in self.merge_commands() { self.fe.info(&n); }
                self.trust_project()
            }
        }
    }

    /// /help: Elisa's commands, then the file commands with where each comes from
    fn help(&self) -> String {
        let mut s = help_text();
        if !self.file_cmds.is_empty() {
            s.push_str("\nFile commands:\n");
            for c in &self.file_cmds { s.push_str(&c.help_line()); s.push('\n'); }
            s.pop();
        }
        if let Some(p) = &self.proj_pending {
            s.push_str(&format!("\nProject commands waiting for trust: {}", p.commands.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(" ")));
        }
        s
    }

    /// every plugin, the user's and the project's
    fn all_plugins(&mut self) -> Vec<plugins::Info> {
        if let Some(h) = self.home.clone() { self.plugin_infos = plugins::load_user(&h, &self.cwd, &|_| false).infos; }
        // a server this session could not start is marked failed, with docker's reason (longrun2 F5)
        plugins::set_live(&mut self.plugin_infos, &self.plugin_servers, &|n| self.mcp.down_reason(n));
        self.plugin_infos.iter().chain(&self.proj_infos).cloned().collect()
    }

    /// the `plugins` event (the app's plugin pane)
    pub(super) fn plugins_event(&mut self) {
        let all = self.all_plugins();
        let list: Vec<serde_json::Value> = all.iter().map(|i| {
            let mut v = plugins::info_json(i);
            // a running server's tool count and cost as this session sees them
            if let Some(a) = v["servers"].as_array_mut() {
                for s in a.iter_mut() {
                    if let Some((n, t)) = s["name"].as_str().and_then(|n| self.mcp.server_tools(n).filter(|_| self.plugin_servers.iter().any(|(_, x)| x == n))) {
                        s["tools"] = json!(n);
                        s["tokens"] = json!(t);
                        s["running"] = json!(true);
                    }
                }
                let (tools, tokens) = a.iter().fold((0, 0), |(x, y), s| (x + s["tools"].as_u64().unwrap_or(0), y + s["tokens"].as_u64().unwrap_or(0)));
                v["tools"] = json!(tools);
                v["tokens"] = json!(tokens);
            }
            v
        }).collect();
        self.fe.event("plugins", &json!({"plugins": list}));
    }

    /// The enabled plugins' servers, rules and commands brought in line with what is installed: servers of plugins
    /// that are gone or off stop, new ones start (`touched`: that plugin's servers restart either way).
    fn reload_plugins(&mut self, touched: Option<&str>) {
        let Some(home) = self.home.clone() else { return };
        let config_names: Vec<String> = self.cfg.mcp.iter().map(|c| c.name.clone()).collect();
        let running_other: Vec<String> = self.mcp_names().into_iter().filter(|n| !self.plugin_servers.iter().any(|(_, s)| s == n)).collect();
        let taken = |n: &str| config_names.iter().any(|c| c == n) || running_other.iter().any(|c| c == n) || crate::config::BUILTIN_SERVERS.contains(&n);
        let l = plugins::load_user(&home, &self.cwd, &taken);
        let keep: Vec<(String, crate::mcp_host::ServerCfg)> = l.servers;
        // stop: plugin servers no longer wanted, and the touched plugin's
        let stop: Vec<String> = self.plugin_servers.iter()
            .filter(|(p, s)| Some(p.as_str()) == touched || !keep.iter().any(|(kp, c)| kp == p && &c.name == s))
            .map(|(_, s)| s.clone()).collect();
        if !stop.is_empty() { self.mcp.remove(&stop); }
        let own: Vec<String> = tools::defs_for(true).into_iter().map(|d| d.name).collect();
        let start: Vec<crate::mcp_host::ServerCfg> = keep.iter().map(|(_, c)| c.clone()).collect();
        for (warn, n) in self.mcp.extend(&start, &|n| own.iter().any(|o| o == n)) { if warn { self.fe.error(&n) } else { self.fe.info(&n) } }
        self.plugin_servers = keep.iter().map(|(p, c)| (p.clone(), c.name.clone())).collect();
        self.cfg.perms.rules.retain(|r| !r.source.starts_with("plugin "));
        for (lv, p, src) in &l.rules { if let Err(e) = self.cfg.perms.add(*lv, p, src) { self.fe.info(&format!("{src}: rule {p:?} skipped: {e}")); } }
        for n in &l.notes { self.fe.info(n); }
        self.cmd_plugins = l.commands;
        self.plugin_infos = l.infos;
        for n in self.merge_commands() { self.fe.info(&n); }
        self.rebuild_system();
        self.offer_plugin_images();
    }

    pub(super) fn mcp_names(&self) -> Vec<String> { self.mcp.costs().into_iter().map(|(n, _)| n).collect() }

    /// Plugins changed outside this session (another window, `elisa plugin remove`): when the state file's stamp moved,
    /// the plugins are loaded again as `/plugin enable` / `disable` would — servers of removed or switched-off plugins
    /// stop and their tools, rules and commands go; newly switched-on ones start — with a line for each. Cheap when
    /// nothing changed (one stat). Run before each turn and before a plugin tool call (longrun2 F4).
    pub(super) fn sync_plugins(&mut self) {
        let Some(home) = self.home.clone() else { return };
        let now = plugins::state_stamp(&home);
        if now == self.plugin_stamp { return; }
        self.plugin_stamp = now;
        let on = |s: &Self| s.plugin_infos.iter().filter(|i| i.enabled && i.trusted).map(|i| i.name.clone()).collect::<Vec<_>>();
        let before = on(self);
        let owned: Vec<(String, String)> = self.mcp.defs().into_iter().filter_map(|d| {
            let srv = self.mcp.server_of(&d.name)?;
            self.plugin_servers.iter().find(|(_, s)| s == srv).map(|(p, _)| (d.name.clone(), p.clone()))
        }).collect();
        self.reload_plugins(None);
        let after = on(self);
        let gone: Vec<&String> = before.iter().filter(|p| !after.contains(p)).collect();
        self.plugin_gone_tools.extend(owned.into_iter().filter(|(_, p)| gone.contains(&p)));
        let mcp = &self.mcp;
        self.plugin_gone_tools.retain(|(t, _)| !mcp.owns(t));
        let new: Vec<&String> = after.iter().filter(|p| !before.contains(p)).collect();
        for p in &gone { self.fe.info(&format!("plugin {p} was removed or switched off outside this session (another Elisa window or `elisa plugin`): its servers stopped, and its tools, rules and commands are off")); }
        for p in &new { self.fe.info(&format!("plugin {p} was switched on outside this session: its servers, rules and commands are in use from now on")); }
        if !gone.is_empty() || !new.is_empty() { self.plugins_event(); }
    }

    /// A call to a plugin server's tool whose plugin was removed or switched off elsewhere (checked just now): why it
    /// does not run. None: the tool is not a plugin's, or its plugin is still on.
    pub(super) fn plugin_tool_gone(&mut self, name: &str) -> Option<String> {
        let plugin_tool = self.mcp.server_of(name).is_some_and(|srv| self.plugin_servers.iter().any(|(_, s)| s == srv));
        if plugin_tool { self.sync_plugins(); }
        let (_, plugin) = self.plugin_gone_tools.iter().find(|(t, _)| t == name)?;
        Some(format!("`{name}` belongs to plugin {plugin}, which was removed or switched off outside this session; its tools are off and nothing ran"))
    }

    /// one plugin operation's outcome: the app's `plugin_result` event, the audit line, and a line for the user
    fn plugin_result(&mut self, op: &str, name: &str, r: Result<String, String>) { self.plugin_result_working(op, name, r, true) }

    /// `plugin_result`; `working` false: done, but a server did not start (said as an error; `"working": false`)
    fn plugin_result_working(&mut self, op: &str, name: &str, r: Result<String, String>, working: bool) {
        let (ok, text) = match &r { Ok(t) => (true, t.clone()), Err(e) => (false, e.clone()) };
        plugins::audit(&self.audit, op, name, if ok && working { "ok" } else if text == DECLINED { "declined" } else { "error" }, if ok && working { "" } else { &text });
        if ok && working { self.fe.info(&text) } else { self.fe.error(&text) }
        self.fe.event("plugin_result", &json!({"op": op, "name": name, "ok": ok, "working": ok && working, "text": text}));
        // this session's own change is not news at the next turn
        self.plugin_stamp = self.home.as_deref().and_then(plugins::state_stamp);
        if ok { self.plugins_event(); }
    }

    /// the setup screen: the preview text, the app's `plugin_preview` event, then yes / no. Err: why not.
    fn plugin_setup(&mut self, op: &str, m: &plugins::Manifest, p: &plugins::Preview) -> Result<(), String> {
        self.fe.info(&p.text);
        self.fe.event("plugin_preview", &json!({"op": op, "name": m.name, "preview": p.json, "text": p.text}));
        if !p.problems.is_empty() { return Err(format!("plugin {}: cannot {op} now — {}", m.name, p.problems.join("; "))); }
        if !self.interactive { return Err(format!("plugin {}: a person has to approve the setup (run /plugin {op} in an interactive session, or elisa plugin {op} … --yes)", m.name)); }
        let q = match op { "update" => format!("Update plugin {} to {}?", m.name, m.version), "trust" => format!("Trust plugin {}'s current files?", m.name),
            _ => format!("Install plugin {} {}?", m.name, m.version) };
        if self.fe.ask_yes_no(&format!("plugin {op} {}", m.name), &q) { Ok(()) } else { Err(DECLINED.into()) }
    }

    /// after an install or update: the servers started once to learn their tools and cost (written to the lock)
    /// … and the closing line (`plugins::outcome_line`): (every server started, the text)
    fn plugin_probe(&mut self, name: &str, done: &str) -> (bool, String) {
        let Some(home) = self.home.clone() else { return (true, done.to_string()) };
        let pr = plugins::probe(&home, &self.cwd, name);
        let (ok, line) = plugins::outcome_line(done, &pr);
        let mut lines: Vec<String> = pr.into_iter().map(|(s, r)| match r {
            Ok((n, t)) => format!("{s}: {n} tool{} · ~{} tokens", if n == 1 { "" } else { "s" }, ui::k(t)),
            Err(e) => format!("{s}: did not start: {e}"),
        }).collect();
        lines.push(line);
        (ok, lines.join("\n"))
    }

    // ---------- sandbox images (`images.rs`) ----------

    /// the `sandbox_images` event (the app's list); quiet when docker cannot be reached
    fn sandbox_event(&mut self) {
        let Some(home) = self.home.clone() else { return };
        match images::status(&home, Some(&self.cwd)) {
            Ok(rows) => self.fe.event("sandbox_images", &images::status_json(&rows, &home)),
            Err(e) => self.fe.event("sandbox_images", &json!({"images": [], "error": e})),
        }
    }

    /// Downloads listed images after the user said yes (never for the model, never without a person): the question
    /// (`images::question`, `why` = what needs it), then each `docker pull` with its progress, cancellable with Esc /
    /// Ctrl-C, every outcome in the audit log. Ok: every image is there now.
    pub(super) fn pull_images(&mut self, imgs: &[&'static images::Img], why: Option<String>) -> Result<(), String> {
        let Some(home) = self.home.clone() else { return Err("no home folder".into()) };
        if imgs.is_empty() { return Ok(()); }
        // no Colima: say how to start it (Elisa never does) before anyone is asked anything
        if let Err(e) = plugins::Docker::find(&home) { self.fe.error(&e); return Err(e); }
        let names = imgs.iter().map(|i| i.image).collect::<Vec<_>>().join(" ");
        if !self.interactive {
            let e = format!("downloading {names} needs a person to say yes: /sandbox pull in an interactive session, or elisa sandbox pull {names} --yes");
            for i in imgs { images::audit(&self.audit, i.image, "denied", "non-interactive"); }
            return Err(e);
        }
        let q = images::question(imgs, why.as_deref());
        if imgs.len() > 1 { self.fe.info(&imgs.iter().map(|i| format!("  {} (~{} MB) — {}", i.image, i.mb, i.purpose())).collect::<Vec<_>>().join("\n")); }
        if !self.fe.ask_yes_no(&format!("sandbox pull {names}"), &q) {
            for i in imgs { images::audit(&self.audit, i.image, "declined", ""); }
            self.fe.info("nothing was downloaded");
            return Err("declined".into());
        }
        for i in imgs {
            let fe: *mut (dyn Frontend + '_) = &mut *self.fe;
            let mut last = String::new();
            let t0 = Instant::now();
            // SAFETY: `fe` is used only inside these callbacks while `pull` runs on this thread; nothing else touches
            // `self.fe` meanwhile
            let r = images::pull(&home, i, &|| CANCEL.load(Ordering::SeqCst), &mut |p| unsafe {
                if p.label() != last {
                    last = p.label();
                    (*fe).progress(Some(&Progress { label: last.clone(), frac: (p.layers > 0).then(|| p.done as f32 / p.layers as f32), elapsed_s: p.elapsed_s, done: None }));
                }
                (*fe).event("image_pull", &p.json("pulling"));
            }, &mut |q| unsafe { (*fe).ask_yes_no(&format!("sandbox pull {}", i.image), q) });
            let label = format!("downloading {}", i.image);
            let done = |t: &str| Progress { label: label.clone(), frac: Some(1.0), elapsed_s: t0.elapsed().as_secs_f64(), done: Some(t.to_string()) };
            let (state, text, ok) = match r {
                Ok(images::Pulled::Ok(t)) => { images::audit(&self.audit, i.image, "ok", &t); ("done", t, true) }
                Ok(images::Pulled::Kept(t)) => { images::audit(&self.audit, i.image, "kept", &t); ("kept", t, false) }
                Ok(images::Pulled::Cancelled) => { images::audit(&self.audit, i.image, "cancelled", ""); ("cancelled", format!("download of {} cancelled", i.image), false) }
                Err(e) => { images::audit(&self.audit, i.image, "error", &e); ("failed", e, false) }
            };
            self.fe.progress(Some(&done(state)));
            self.fe.event("image_pull", &json!({"image": i.image, "state": state, "text": text}));
            if ok { self.fe.info(&text) } else { self.fe.error(&text) }
            if !ok { self.sandbox_event(); return Err(text); }
        }
        self.sandbox_event();
        Ok(())
    }

    /// `/sandbox [list] | pull NAME|IMAGE|stable|project|all`
    fn sandbox_command(&mut self, arg: &str) {
        let (op, rest) = arg.split_once(char::is_whitespace).map(|(a, b)| (a, b.trim())).unwrap_or((arg, ""));
        let Some(home) = self.home.clone() else { self.fe.error("no home folder"); return };
        match op {
            "" | "list" => match images::status(&home, Some(&self.cwd)) {
                Ok(rows) => { self.fe.info(&images::status_text(&rows)); self.fe.event("sandbox_images", &images::status_json(&rows, &home)); }
                Err(e) => { self.fe.error(&e); self.fe.event("sandbox_images", &json!({"images": [], "error": e})); }
            },
            "pull" => {
                let d = match plugins::Docker::find(&home) { Ok(d) => d, Err(e) => { self.fe.error(&e); return } };
                let present = |i: &images::Img| d.image_info(i.image).ok().flatten().is_some();
                match images::targets(rest, Some(&self.cwd), &present) {
                    Ok((v, _)) if v.is_empty() => self.fe.info("nothing to download: those images are on this machine already"),
                    Ok((v, again)) => {
                        let why = (again && v.len() == 1 && present(v[0]))
                            .then(|| format!("{} is on this machine already; downloading it again updates it. The {} uses", v[0].image, v[0].purpose()));
                        let _ = self.pull_images(&v, why);
                    }
                    Err(e) => self.fe.error(&e),
                }
            }
            _ => self.fe.error("usage: /sandbox [list] | pull NAME|IMAGE|stable|project|all"),
        }
    }

    /// Plugin servers that did not start because a listed image is missing: one question for those images, then
    /// the servers start again (interactive sessions only; elsewhere the startup note says how to get them).
    /// The MCP server of tool `name` started for its first call (`mcp_host::Host::ensure`), before the approval: a
    /// tool list that differs from its pin shows its warning here and the call asks as usual. A plugin server whose
    /// image is missing: one question, then one more try. Err: why it could not start (the tool's result).
    pub(super) fn mcp_start(&mut self, name: &str) -> Result<(), String> {
        let mut r = self.mcp.ensure(name);
        if r.is_err() && self.mcp.take_image_start_failed() {
            self.offer_plugin_images();
            if self.mcp.owns(name) { r = self.mcp.ensure(name); }
        }
        for n in self.mcp.take_notes() { self.fe.error(&n); }
        r
    }

    pub(super) fn offer_plugin_images(&mut self) {
        if !self.interactive { return; }
        let down = self.mcp.down_for_image();
        if down.is_empty() { return; }
        let mut imgs: Vec<&'static images::Img> = Vec::new();
        for (_, i) in &down { if !imgs.contains(i) { imgs.push(i); } }
        let who = down.iter().map(|(c, _)| c.name.clone()).collect::<Vec<_>>().join(", ");
        let why = (imgs.len() == 1).then(|| format!("Plugin server {who} needs"));
        if self.pull_images(&imgs, why).is_err() { return; }
        let cfgs: Vec<crate::mcp_host::ServerCfg> = down.into_iter().map(|(c, _)| c).collect();
        let own: Vec<String> = tools::defs_for(true).into_iter().map(|d| d.name).collect();
        for (warn, n) in self.mcp.extend(&cfgs, &|n| own.iter().any(|o| o == n)) { if warn { self.fe.error(&n) } else { self.fe.info(&n) } }
        self.rebuild_system();
        self.plugins_event();
    }

    /// A plugin preview that lists missing images: shown, then one question to download them. true: they are
    /// there now (preview again).
    fn offer_preview_images(&mut self, op: &str, name: &str, p: &plugins::Preview) -> bool {
        if p.missing_images.is_empty() || !self.interactive { return false; }
        let imgs: Vec<&'static images::Img> = p.missing_images.iter().filter_map(|i| images::listed(i)).collect();
        if imgs.len() != p.missing_images.len() { return false; }
        self.fe.info(&p.text);
        self.fe.event("plugin_preview", &json!({"op": op, "name": name, "preview": p.json, "text": p.text}));
        let why = (imgs.len() == 1).then(|| format!("Plugin {name} needs"));
        self.pull_images(&imgs, why).is_ok()
    }

    /// `/plugin list | add PATH | remove NAME | enable NAME | disable NAME | update NAME | trust NAME|project`
    fn plugin_command(&mut self, arg: &str) {
        let (op, rest) = arg.split_once(char::is_whitespace).map(|(a, b)| (a, b.trim())).unwrap_or((arg, ""));
        let Some(home) = self.home.clone() else { self.fe.error("no home folder: plugins live in ~/.elisa/plugins"); return };
        let need = |s: &mut Self, what: &str| { s.fe.error(&format!("usage: /plugin {op} {what}")); };
        match op {
            "" | "list" => {
                let all = self.all_plugins();
                self.fe.info(&plugins::list_text(&all));
                self.plugins_event();
            }
            "add" => {
                if rest.is_empty() { return need(self, "PATH"); }
                let (m, src, p) = match plugins::prepare_add(&home, &self.cwd, rest) { Ok(x) => x, Err(e) => return self.plugin_result("add", rest, Err(e)) };
                let (m, src, p) = if self.offer_preview_images("add", &m.name, &p) {
                    match plugins::prepare_add(&home, &self.cwd, rest) { Ok(x) => x, Err(e) => return self.plugin_result("add", rest, Err(e)) }
                } else { (m, src, p) };
                if let Err(e) = self.plugin_setup("add", &m, &p) { return self.plugin_result("add", &m.name, Err(e)); }
                let fe = &mut *self.fe;
                let mut r = plugins::install(&home, &src, &m, &mut |l| fe.info(l));
                // the image went missing between the preview and the install: offer it once, then try again
                if let Some(img) = r.as_ref().err().and_then(|e| images::marked(e)) {
                    if self.pull_images(&[img], Some(format!("Plugin {} needs", m.name))).is_ok() {
                        let fe = &mut *self.fe;
                        r = plugins::install(&home, &src, &m, &mut |l| fe.info(l));
                    }
                }
                if let Err(e) = r { return self.plugin_result("add", &m.name, Err(e)); }
                let (working, t) = self.plugin_probe(&m.name, &format!("plugin {} {} installed and on", m.name, m.version));
                self.reload_plugins(Some(&m.name));
                self.plugin_result_working("add", &m.name, Ok(t), working);
            }
            "remove" => {
                if rest.is_empty() { return need(self, "NAME"); }
                let mine: Vec<String> = self.plugin_servers.iter().filter(|(p, _)| p == rest).map(|(_, s)| s.clone()).collect();
                if !mine.is_empty() { self.mcp.remove(&mine); }
                let r = plugins::remove(&home, rest).map(|notes| {
                    let mut t = format!("plugin {rest} removed");
                    for n in notes { t.push_str(&format!("\n{n}")); }
                    t
                });
                self.reload_plugins(None);
                self.plugin_result("remove", rest, r);
            }
            "enable" | "disable" => {
                if rest.is_empty() { return need(self, "NAME"); }
                let r = plugins::set_enabled(&home, rest, op == "enable");
                if r.is_ok() { self.reload_plugins(Some(rest)); }
                self.plugin_result(op, rest, r);
            }
            "update" => {
                if rest.is_empty() { return need(self, "NAME"); }
                let (m, src, p) = match plugins::prepare_update(&home, rest) { Ok(x) => x, Err(e) => return self.plugin_result("update", rest, Err(e)) };
                let (m, src, p) = if self.offer_preview_images("update", rest, &p) {
                    match plugins::prepare_update(&home, rest) { Ok(x) => x, Err(e) => return self.plugin_result("update", rest, Err(e)) }
                } else { (m, src, p) };
                if let Err(e) = self.plugin_setup("update", &m, &p) { return self.plugin_result("update", rest, Err(e)); }
                let mine: Vec<String> = self.plugin_servers.iter().filter(|(pl, _)| pl == rest).map(|(_, s)| s.clone()).collect();
                if !mine.is_empty() { self.mcp.remove(&mine); }
                let fe = &mut *self.fe;
                let r = plugins::update(&home, &src, &m, &mut |l| fe.info(l));
                let (working, t) = if r.is_ok() { self.plugin_probe(rest, &format!("plugin {rest} updated to {}", m.version)) } else { (true, String::new()) };
                self.reload_plugins(Some(rest));
                self.plugin_result_working("update", rest, r.map(|()| t), working);
            }
            "trust" if rest == "project" => {
                if self.proj_pending.is_none() { self.fe.info("this project has no commands or plugins waiting for trust"); return; }
                self.proj_declined = false;
                self.trust_project();
            }
            "trust" => {
                if rest.is_empty() { return need(self, "NAME | project"); }
                let (m, p) = match plugins::prepare_trust(&home, rest) {
                    Ok(x) => x,
                    Err(e) => {
                        // a plugin.toml that does not parse cannot be trusted: offer the removal, or keep it off
                        let broken = e.contains(plugins::UNPARSABLE);
                        self.plugin_result("trust", rest, Err(e));
                        if broken && self.interactive && self.fe.ask_yes_no(&format!("plugin remove {rest}"), &format!("Remove plugin {rest} now? (no keeps it off)")) {
                            self.plugin_command(&format!("remove {rest}"));
                        } else if broken { self.fe.info(&format!("plugin {rest} stays off")); }
                        return;
                    }
                };
                let (m, p) = if self.offer_preview_images("trust", rest, &p) {
                    match plugins::prepare_trust(&home, rest) { Ok(x) => x, Err(e) => return self.plugin_result("trust", rest, Err(e)) }
                } else { (m, p) };
                if let Err(e) = self.plugin_setup("trust", &m, &p) { return self.plugin_result("trust", rest, Err(e)); }
                let r = plugins::trust(&home, rest);
                if r.is_ok() { self.reload_plugins(Some(rest)); }
                self.plugin_result("trust", rest, r);
            }
            _ => self.fe.error("usage: /plugin list | add PATH | remove NAME | enable NAME | disable NAME | update NAME | trust NAME|project"),
        }
    }

    /// Returns false to quit.
    pub(super) fn command(&mut self, line: &str) -> bool {
        let cmd = line.split_whitespace().next().unwrap_or("");
        match cmd {
            "/help" => { let h = self.help(); self.fe.info(&h) }
            "/plugin" | "/plugins" => self.plugin_command(line[cmd.len()..].trim()),
            "/sandbox" => self.sandbox_command(line[cmd.len()..].trim()),
            "/exit" | "/quit" => return false,
            "/clear" | "/compact" => {
                if self.history.is_empty() { self.fe.info("history is empty; notes unchanged"); return true; }
                // the conversation's KV, before the summary call and the fresh start replace it (/resume brings it back)
                if cmd == "/clear" { self.slot_save("/clear"); }
                // the text estimate, like the size after (a server count against an estimate would compare apples
                // with pears)
                let before = self.fixed() + estimate_messages(&self.history);
                // same progress job as the automatic fold ("summarizing …"): the status line shows it and input pauses
                let turns = self.history.iter().filter(|m| m.role == Role::User && m.content.iter().any(|b| matches!(b, Block::Text { .. }))).count();
                let earlier = if self.notes.is_some() { " + earlier notes" } else { "" };
                let (label, t0) = (format!("summarizing {turns} turn{} ({}){earlier}", if turns == 1 { "" } else { "s" }, ui::k(before)), Instant::now());
                let prog = |frac: f32, done: Option<String>| Progress { label: label.clone(), frac: Some(frac), elapsed_s: t0.elapsed().as_secs_f64(), done };
                self.fe.progress(Some(&prog(0.0, None)));
                let r = self.summarise("[summarising…]");
                let done = match &r { Ok(_) => "saved summary".to_string(), Err(_) => "summary failed".to_string() };
                self.fe.progress(Some(&prog(1.0, Some(done))));
                match r {
                    Ok(n) => {
                        let old = self.notes.clone();
                        // merged into the earlier notes (the folds' summaries and cards stay), its claims checked
                        // and the files changed this session taken from Elisa's own records (soak: /clear replaced
                        // the notes with one turn's summary that said "No files were touched" after 4 edits)
                        let v = self.verified(&n);
                        let m = self.merge_kept(old.as_deref(), &v);
                        let n = self.finish_notes(m);
                        self.mem_note(&n, usize::MAX);
                        // /clear is a fresh start: the notes become background and the card goes (the notes carry it)
                        let clear = cmd == "/clear";
                        if clear {
                            self.new_memory_session();
                            // the pre-clear conversation stays in its own session file (resumable)
                            context::new_session();
                            self.goal = None;
                            (self.card, self.card_id, self.card_ex) = (None, None, None);
                            self.topic_cards.clear();
                            (self.topic, self.card_req) = (Default::default(), None);
                            self.ledger = context::Ledger::default();
                        }
                        self.card_log.clear();
                        self.history.clear();
                        self.set_notes(Some(n));
                        self.notes_bg = clear;
                        // /clear comes out smaller when only low-value lines need to go; /compact keeps the whole
                        // summary (it is the task's memory)
                        let after = if clear { self.shrink_notes(before, old.clone()) } else { self.estimate() };
                        if let Some(notes) = self.notes.clone() {
                            self.fe.notes_saved(&notes);
                            self.watch_note("fold", &notes);
                        }
                        // `compacted` carries the new context size: the status bars show it now, not at the next reply
                        self.fe.compacted(before, after);
                        if clear {
                            let kept = match &self.notes {
                                None => "no notes kept: the conversation was shorter than any summary",
                                Some(_) if after >= before => "notes saved to .elisa/notes/, whole: the conversation was shorter than its facts, decisions and file changes",
                                Some(_) => "notes saved to .elisa/notes/",
                            };
                            self.fe.info(&format!("[cleared: {} → {} tokens; {kept}]", ui::k(before), ui::k(after)));
                        }
                    }
                    Err(e) => self.fe.error(&format!("summary failed: {e}. Old notes kept, history kept (use /reset to wipe without a summary).")),
                }
            }
            "/reset" => {
                // a fresh start also drops the carried state card, or the next request still continues the old task
                self.history.clear(); self.real = None; self.fold_reset();
                self.drop_card();
                self.goal = None;
                self.notes_bg = true;
                self.fe.info("history and state card emptied (notes kept)");
            }
            "/context" => {
                let h = estimate_messages(&self.history);
                let msg = format!(
                    "system {} · tools {} · history {} ({} msgs) · total {} / limit {} (ctx {} - output {}) · fold at {}",
                    ui::k(self.system_tokens), ui::k(self.tools_tokens), ui::k(h), self.history.len(), ui::k(self.estimate()),
                    ui::k(self.limit()), ui::k(self.max_ctx), ui::k(self.reserve()), ui::k(self.fold_point()));
                if let Some(l) = self.ast_stats.line() { self.fe.info(&l); }
                self.fe.info(&msg);
                let by: Vec<String> = self.tool_costs().iter().map(|(n, t)| format!("{n} {}", ui::k(*t))).collect();
                self.fe.info(&format!("tools by owner (this turn): {} · /mcp for each server", by.join(" · ")));
                self.fe.info(&format!("route (last turn): {} · config route = \"{}\"", self.route.describe(), self.cfg.route));
                if self.router.on() {
                    let said = self.routed.as_ref().map_or("no answer: word rules used".to_string(), |a| a.describe());
                    self.fe.info(&format!("intent router (last turn): {said}"));
                }
                if self.cfg.route == "auto" && self.mcp.has_gated() {
                    let on = |srv: &str| if self.tools.iter().any(|t| self.mcp.gated_server(&t.name) == Some(srv)) { "offered" } else { "not offered" };
                    self.fe.info(&format!("documents tools {} · security tools {} this turn (routed: only on turns about them)", on("docs"), on("sec")));
                } else if self.cfg.gate_tools == "auto" && self.mcp.has_gated() {
                    self.fe.info(&format!("documents tools {} · security tools {} this turn (gate_tools = \"auto\": only on turns about them)",
                        if self.gate.0 { "offered" } else { "not offered" }, if self.gate.1 { "offered" } else { "not offered" }));
                }
                if self.mcp.reason_tool("think") {
                    self.fe.info(&format!("reasoning tools (think, steps, step_done, calc): {} this turn (reason = \"{}\")",
                        if self.reason_on { "offered" } else { "not offered" }, self.cfg.reason));
                }
                let probe = [Message::user_text(".")];
                let count = |m: &[Message]| self.client.count_tokens(&Request { system: &self.system, tools: &self.tools, messages: m, max_tokens: 1 });
                let msg = match (count(&probe), if self.history.is_empty() { Ok(0) } else { count(&self.history) }) {
                    (Ok(f), Ok(a)) => format!("server count: system+tools+empty turn {f} · whole prompt {a}"),
                    (Err(e), _) | (_, Err(e)) => format!("server count unavailable: {e}"),
                };
                self.fe.info(&msg);
            }
            "/state" => {
                if self.topic_cards.is_empty() { self.fe.info("no state card yet"); return true; }
                let docs: Vec<String> = self.kept_docs.iter().map(|k| k.doc.name.clone()).collect();
                // the topics: each document (attach order), then the cards about no document
                let mut keys: Vec<Option<String>> = docs.iter().cloned().map(Some).collect();
                if self.topic_cards.iter().any(|c| c.doc.as_ref().is_none_or(|d| !docs.contains(d))) { keys.push(None); }
                let active = self.topic.card;
                let mut out = Vec::new();
                for k in keys {
                    let mine: Vec<(usize, &crate::topics::Card)> = self.topic_cards.iter().enumerate()
                        .filter(|(_, c)| match &k { Some(d) => c.doc.as_ref() == Some(d), None => c.doc.as_ref().is_none_or(|d| !docs.contains(d)) }).collect();
                    let doc_on = k.as_ref().is_some_and(|d| self.topic.doc.and_then(|i| docs.get(i)) == Some(d));
                    let head = match &k { Some(d) => format!("topic: {d}{}", if doc_on { "  (active)" } else { "" }), None => "topic: no document".to_string() };
                    let mut t = vec![head];
                    if mine.is_empty() { t.push("  (no card yet)".into()); }
                    for (i, c) in mine {
                        let n = c.id.map_or("card".to_string(), |id| format!("card #{id}"));
                        t.push(format!("  {n}{}:", if active == Some(i) { " (active)" } else { "" }));
                        t.extend(c.text.lines().map(|l| format!("    {l}")));
                    }
                    out.push(t.join("\n"));
                }
                let why = format!("last turn's pick: {}", self.topic.why);
                self.fe.info(&format!("{}\n\n{why}", out.join("\n\n")));
            }
            "/trust" => {
                let name = line[cmd.len()..].trim();
                let changed_plugins: Vec<String> = self.home.clone().map(|h| self.all_plugins().into_iter()
                    .filter(|i| i.source == "user" && !i.trusted && plugins::changed(&h, &i.name)).map(|i| i.name).collect()).unwrap_or_default();
                if name.is_empty() {
                    let u = self.mcp.untrusted_servers();
                    self.fe.info(&if u.is_empty() { "every MCP server's tool list matches its pin (~/.elisa/mcp-pins.json)".to_string() }
                        else { format!("tool list changed since it was pinned: {} · /trust NAME accepts the new list", u.join(", ")) });
                    if !changed_plugins.is_empty() { self.fe.info(&format!("plugin files changed since you approved them: {} · /trust NAME reviews them", changed_plugins.join(", "))); }
                } else if changed_plugins.iter().any(|p| p == name) {
                    self.plugin_command(&format!("trust {name}"));
                } else {
                    match self.mcp.trust(name) {
                        Ok(m) => { self.audit.note("trust", &[("server", name), ("outcome", "ok")]); self.fe.info(&m); }
                        Err(e) => self.fe.error(&e),
                    }
                }
            }
            "/audit" => self.audit_command(line[cmd.len()..].trim()),
            "/mcp" => self.mcp_command(line[cmd.len()..].trim()),
            "/cache" => match crate::cache::command(line[cmd.len()..].trim()) {
                Ok(t) => self.fe.info(&t),
                Err(e) => self.fe.error(&e),
            },
            "/allow" => {
                if line[cmd.len()..].trim() == "clear" {
                    self.always.clear();
                    match self.store.save_allowed(&self.always) {
                        Ok(()) => self.fe.info("always-allow list cleared: every tool asks again"),
                        Err(e) => self.fe.error(&format!("could not clear .elisa/allow: {e}")),
                    }
                } else {
                    // MCP keys (`mcp:web`) read as "MCP server web"
                    let show = |k: &str| match k.strip_prefix(tools::MCP_KEY) { Some(srv) => format!("MCP server {srv}"), None => k.to_string() };
                    let mut v: Vec<String> = self.always.iter().map(|k| show(k)).collect();
                    v.sort_unstable();
                    let auto = self.cfg.auto_approve.iter().map(|k| show(k)).collect::<Vec<_>>().join(", ");
                    self.fe.info(&if v.is_empty() { format!("no tools always allowed in this project (never ask: {auto})") }
                        else { format!("always allowed in this project: {} (never ask: {auto}) · /allow clear to reset", v.join(", ")) });
                    let granted: Vec<String> = self.outside_ok.iter().map(|(p, w)| format!("{}{}", p.display(), if *w { " (read + write)" } else { " (read)" })).collect();
                    self.fe.info(&format!("outside the project folder: {} (config outside){}", self.outside_mode.word(),
                        if granted.is_empty() { String::new() } else { format!(" · allowed for this session: {}", granted.join(", ")) }));
                }
            }
            "/notes" => match self.notes.clone() {
                Some(n) => self.fe.info(&n),
                None => self.fe.info("no notes"),
            },
            "/forget" => {
                // the notes' "Facts to remember" were pinned into memory when the notes were saved: drop those too,
                // or the next request still carries them in [state]
                self.set_notes(None);
                self.drop_card();
                let n = self.mem.as_mut().map(|m| m.db.unpin_all(m.session, &m.project));
                // what earlier chats in this project left in memory (their turns, notes, cards) is not recalled again:
                // otherwise a new chat asked "what's my favourite colour?" still gets the old answer from memory
                let next = self.mem.as_mut().map(|m| { let _ = m.db.refresh(); m.db.next_id() });
                let recalled = match next.map(|id| self.store.save_forgot(id)) {
                    Some(Ok(())) => " · memories of earlier chats in this project forgotten (saved conversations kept)",
                    Some(Err(e)) => { self.fe.error(&format!("forgetting memories failed: {e}")); "" }
                    None => "",
                };
                match n {
                    Some(Ok(n)) => self.fe.info(&format!("notes deleted · {n} pinned facts unpinned{recalled}")),
                    Some(Err(e)) => self.fe.error(&format!("notes deleted; unpinning facts failed: {e}{recalled}")),
                    None => self.fe.info("notes deleted"),
                }
            }
            "/resume" => {
                let list = self.store.list_sessions();
                if list.is_empty() { self.fe.info("no saved session"); return true; }
                let Some(i) = self.fe.pick_session(&list).filter(|&i| i < list.len()) else { self.fe.info("resume cancelled"); return true; };
                match self.store.load_session(&list[i].path).map(context::loaded) {
                    Some(h) => {
                        // the session left behind keeps its KV too (llama.cpp slot save), then the resumed one's comes back
                        self.slot_save("session switch");
                        self.slot_restore(&list[i].path.clone());
                        self.fe.info(&format!("resumed \"{}\": {} messages restored (~{} tokens)", list[i].title, h.len(), ui::k(estimate_messages(&h))));
                        // the session left behind keeps its own file and notes; the resumed work goes on in a new one
                        context::new_session();
                        self.history = h;
                        self.real = None;
                        // back to that work: its card and notes are the task again
                        let notes = self.store.load_session_notes(&list[i].path);
                        self.adopt_notes(notes);
                        self.restore_docs(Some(list[i].ts));
                        self.load_topics(Some(list[i].ts));
                        self.carry_card();
                        self.notes_bg = false;
                        self.show_history();
                    }
                    None => self.fe.error(&format!("could not load {}", list[i].path.display())),
                }
            }
            "/pin" => {
                let fact = line[cmd.len()..].trim();
                if fact.is_empty() { self.fe.info("usage: /pin <fact>   e.g. /pin the release branch is trunk"); return true; }
                let r = self.mem.as_mut().map(|m| m.db.pin_fact(m.session, &crate::secrets::redact(fact).0));
                match r {
                    None => self.fe.error("memory is off: nothing to pin into"),
                    Some(Ok(id)) => { self.unindexed += 1; self.fe.info(&format!("pinned fact #{id}: {fact} (carried in [state] from the next fresh context)")); }
                    Some(Err(e)) => self.fe.error(&format!("memory: {e}")),
                }
            }
            "/watcher" => {
                let w = self.watch.fetch_now();
                self.watch_poll();
                self.fe.watcher(w.as_ref());
            }
            "/full" => self.full(),
            "/copy" => self.copy_last(),
            "/model" => self.model_command(line[cmd.len()..].trim()),
            "/chat" | "/code" | "/mode" => self.fe.info("modes were removed: each turn gets the tools its words call for (config route = \"auto\"; /context shows the last turn's groups)"),
            "/water" | "/feed" | "/food" | "/play" | "/toy" | "/poke" => self.fe.info("Elisa's character lives in the full-screen view (not in plain mode)"),
            "/stats" => {
                let file = self.store.dir.join(metrics::FILE);
                if self.cfg.metrics { self.fe.info(self.stats.text(&file).trim_end()); }
                else { self.fe.info("metrics are off (config `metrics = false`)"); }
                self.fe.info(&crate::cache::stats_line());
            }
            "/yolo" => { self.yolo = !self.yolo; self.fe.info(if self.yolo { "approve-all ON" } else { "approve-all OFF" }); }
            _ => self.fe.error(&format!("unknown command {cmd} (try /help)")),
        }
        true
    }

    /// /full: the full model's answer to the newest reply its audit found changed (of this session's replies, else any).
    fn full(&mut self) {
        let _ = self.watch.fetch_now();
        self.watch_poll();
        if self.watch.off() { return self.fe.info("watcher: not available on this gateway"); }
        match self.watch.last_full() {
            Some(a) => {
                let at = a.diverge_at.map_or(String::new(), |t| format!(" · differs at token {t}"));
                let dom = if a.domain.is_empty() { String::new() } else { format!(" · {}", a.domain) };
                self.fe.full_answer(&format!("full model{dom}{at}"), a.full_answer.as_deref().unwrap_or(""));
            }
            None => self.fe.info("no audit with a full-model answer yet"),
        }
    }

    /// /copy: the last reply's text to the clipboard (pbcopy).
    fn copy_last(&mut self) {
        let Some(t) = self.history.iter().rev().find(|m| m.role == Role::Assistant).map(texts).filter(|t| !t.trim().is_empty()) else {
            return self.fe.info("no reply to copy yet");
        };
        let r = std::process::Command::new("pbcopy").stdin(std::process::Stdio::piped()).spawn().and_then(|mut c| {
            use std::io::Write;
            c.stdin.take().map_or(Ok(()), |mut i| i.write_all(t.trim().as_bytes()))?;
            c.wait()
        });
        match r {
            Ok(s) if s.success() => self.fe.info(&format!("copied the last reply ({} chars)", t.trim().chars().count())),
            Ok(s) => self.fe.error(&format!("copy failed: pbcopy {s}")),
            Err(e) => self.fe.error(&format!("copy failed: pbcopy: {e}")),
        }
    }
}

/// what `plugin_setup` returns when the user said no
const DECLINED: &str = "nothing was changed: the setup was declined";

/// a slash command Elisa has built in (a file command of the same name is not loaded)
fn builtin_command(name: &str) -> bool {
    COMMANDS.iter().any(|(c, _)| *c == name) || ["/quit", "/plugins", "/chat", "/code", "/mode", "/food", "/toy"].contains(&name)
}

/// `2 commands · 1 plugin · 3 rules` for the project's trust notice
fn project_summary(p: &plugins::Project) -> String {
    let n = |k: usize, w: &str| format!("{k} {w}{}", if k == 1 { "" } else { "s" });
    let mut v = vec![n(p.commands.len(), "command")];
    if !p.infos.is_empty() { v.push(n(p.infos.len(), "plugin")); }
    if !p.rules.is_empty() { v.push(n(p.rules.len(), "rule")); }
    v.join(" · ")
}
