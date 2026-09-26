//! The visor calls this orchestrator. It does not call a model directly.
//!
//! The orchestrator owns the default catalog id, the catalog and live-net
//! checks, which hosts a catalog id may use, and an expert set when one is
//! marked. The strings below are the in-tree product catalog. They are not
//! imported from hypermesh-host, and this is not a new catalog service.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::prompt::{ExpertAside, ForwardedPrompt, PromptDoor, SupervisorDoor, UnconfiguredDoor};

/// Catalog id used when the caller omits `model`. One string, beside this seam.
pub const DEFAULT_CATALOG_ID: &str = "llama-3.1-8b-q4";

const PRODUCT_CATALOG: &[&str] = &[
    DEFAULT_CATALOG_ID,
    "whisper-small",
    "embed-minilm",
    "qwen-2.5-32b-q4",
    "llama-3.1-70b-q4",
];

#[derive(Clone, Debug)]
pub struct DoorConfig {
    pub default_model: String,
    pub primary_url: Option<String>,
    pub second_url: Option<String>,
    pub routes: Vec<(String, String)>,
    pub experts: Vec<String>,
    pub answers: Vec<(String, String)>,
}

impl Default for DoorConfig {
    fn default() -> Self {
        Self {
            default_model: DEFAULT_CATALOG_ID.to_string(),
            primary_url: None,
            second_url: None,
            routes: Vec::new(),
            experts: Vec::new(),
            answers: Vec::new(),
        }
    }
}

pub(crate) enum Choice {
    Model(String),
    Unknown,
}

pub(crate) enum Dispatch {
    Unmapped,
    Finished {
        result: Result<String, String>,
        door: Option<String>,
        other_expert: Option<ExpertAside>,
    },
}

struct Host {
    name: String,
    door: Arc<dyn PromptDoor>,
}

struct Report {
    door: Mutex<Option<String>>,
    other: Mutex<Option<ExpertAside>>,
}

impl Report {
    fn clear(&self) {
        *self
            .door
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = None;
        *self
            .other
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = None;
    }

    fn set_door(&self, name: &str) {
        *self
            .door
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(name.to_string());
    }

    fn door(&self) -> Option<String> {
        self.door
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    fn take_other(&self) -> Option<ExpertAside> {
        self.other
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
    }
}

enum Plan {
    Unconfigured,
    Unmapped,
    One {
        name: Option<String>,
        door: Arc<dyn PromptDoor>,
    },
    Failover {
        hosts: Vec<Host>,
    },
    Experts {
        first: Host,
        second: Host,
        answer_second: bool,
    },
}

pub(crate) struct Orchestrator {
    default_model: String,
    catalog: HashSet<String>,
    live_net: HashSet<String>,
    doors: Vec<Host>,
    routes: HashMap<String, Vec<String>>,
    experts: HashSet<String>,
    answers: HashMap<String, String>,
    #[cfg(test)]
    checks: Mutex<Vec<&'static str>>,
    report: Arc<Report>,
}

impl Orchestrator {
    pub(crate) fn single(default_model: &str, door: Arc<dyn PromptDoor>) -> Self {
        Self::from_named(
            default_model,
            vec![(String::new(), door)],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    }

    pub(crate) fn unconfigured(default_model: &str) -> Self {
        Self::from_named(
            default_model,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    }

    /// `doors` are `(name, door)`. An empty name is an unnamed single door.
    /// Zero doors stay unconfigured. One door serves every catalog id.
    /// Two doors use `routes` (catalog id, door name) and fail closed when a
    /// model is missing. `experts` marks a catalog id that fans out.
    pub(crate) fn from_named(
        default_model: &str,
        doors: Vec<(String, Arc<dyn PromptDoor>)>,
        routes: Vec<(String, String)>,
        experts: Vec<String>,
        answers: Vec<(String, String)>,
    ) -> Self {
        let default_model = {
            let trimmed = default_model.trim();
            if trimmed.is_empty() {
                DEFAULT_CATALOG_ID.to_string()
            } else {
                trimmed.to_string()
            }
        };
        let mut route_map: HashMap<String, Vec<String>> = HashMap::new();
        for (model, host) in routes {
            let model = model.trim().to_string();
            let host = host.trim().to_string();
            if model.is_empty() || host.is_empty() {
                continue;
            }
            let entry = route_map.entry(model).or_default();
            if !entry.iter().any(|had| had == &host) {
                entry.push(host);
            }
        }
        Self {
            default_model,
            catalog: product_ids(),
            live_net: product_ids(),
            doors: doors
                .into_iter()
                .map(|(name, door)| Host { name, door })
                .collect(),
            routes: route_map,
            experts: experts
                .into_iter()
                .map(|id| id.trim().to_string())
                .filter(|id| !id.is_empty())
                .collect(),
            answers: answers
                .into_iter()
                .map(|(model, host)| (model.trim().to_string(), host.trim().to_string()))
                .filter(|(model, host)| !model.is_empty() && !host.is_empty())
                .collect(),
            #[cfg(test)]
            checks: Mutex::new(Vec::new()),
            report: Arc::new(Report {
                door: Mutex::new(None),
                other: Mutex::new(None),
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_net(mut self, catalog: &[&str], live: &[&str]) -> Self {
        self.catalog = catalog.iter().map(|id| (*id).to_string()).collect();
        self.live_net = live.iter().map(|id| (*id).to_string()).collect();
        self
    }

    #[cfg(test)]
    pub(crate) fn checks(&self) -> Vec<&'static str> {
        self.checks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    /// An omitted model becomes the flagged default and is not looked up.
    /// A caller-set id is checked in the catalog and on the live net, both,
    /// before it is known.
    pub(crate) fn select(&self, requested: Option<String>) -> Choice {
        let Some(id) = requested else {
            return Choice::Model(self.default_model.clone());
        };
        let in_catalog = self.catalog.contains(&id);
        let on_net = self.live_net.contains(&id);
        #[cfg(test)]
        {
            let mut checks = self
                .checks
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            checks.clear();
            checks.push("catalog");
            checks.push("live_net");
        }
        if in_catalog && on_net {
            Choice::Model(id)
        } else {
            Choice::Unknown
        }
    }

    pub(crate) fn dispatch(&self, model: &str, call: &ForwardedPrompt) -> Dispatch {
        self.report.clear();
        match self.plan(model) {
            Plan::Unmapped => Dispatch::Unmapped,
            Plan::Unconfigured => Dispatch::Finished {
                result: UnconfiguredDoor.forward(call),
                door: None,
                other_expert: None,
            },
            Plan::One { name, door } => {
                if let Some(name) = &name {
                    self.report.set_door(name);
                }
                let result = door.forward(call);
                Dispatch::Finished {
                    result,
                    door: self.report.door(),
                    other_expert: None,
                }
            }
            Plan::Failover { hosts } => self.failover(&hosts, call),
            Plan::Experts {
                first,
                second,
                answer_second,
            } => {
                let answer_name = if answer_second {
                    second.name.clone()
                } else {
                    first.name.clone()
                };
                self.report.set_door(&answer_name);
                let fan = ExpertFan {
                    first,
                    second,
                    answer_second,
                    report: Some(Arc::clone(&self.report)),
                };
                let result = fan.forward(call);
                Dispatch::Finished {
                    result,
                    door: self.report.door(),
                    other_expert: self.report.take_other(),
                }
            }
        }
    }

    fn failover(&self, hosts: &[Host], call: &ForwardedPrompt) -> Dispatch {
        let mut last = "prompt door is not configured".to_string();
        let mut door_name = None;
        for host in hosts {
            door_name = Some(host.name.clone());
            self.report.set_door(&host.name);
            match host.door.forward(call) {
                Ok(text) => {
                    return Dispatch::Finished {
                        result: Ok(text),
                        door: door_name,
                        other_expert: None,
                    };
                }
                Err(err) => last = err,
            }
        }
        Dispatch::Finished {
            result: Err(last),
            door: door_name,
            other_expert: None,
        }
    }

    fn plan(&self, model: &str) -> Plan {
        if self.doors.is_empty() {
            return Plan::Unconfigured;
        }
        if self.doors.len() == 1 {
            let host = &self.doors[0];
            let name = if host.name.is_empty() {
                None
            } else {
                Some(host.name.clone())
            };
            return Plan::One {
                name,
                door: Arc::clone(&host.door),
            };
        }
        if self.experts.contains(model) && self.doors.len() >= 2 {
            let answer = self.answers.get(model).map(String::as_str);
            let answer_second = answer == Some(self.doors[1].name.as_str());
            return Plan::Experts {
                first: clone_host(&self.doors[0]),
                second: clone_host(&self.doors[1]),
                answer_second,
            };
        }
        let Some(names) = self.routes.get(model) else {
            return Plan::Unmapped;
        };
        let hosts: Vec<Host> = names
            .iter()
            .filter_map(|name| {
                self.doors
                    .iter()
                    .find(|host| host.name == *name)
                    .map(clone_host)
            })
            .collect();
        if hosts.is_empty() {
            Plan::Unmapped
        } else {
            Plan::Failover { hosts }
        }
    }
}

fn clone_host(host: &Host) -> Host {
    Host {
        name: host.name.clone(),
        door: Arc::clone(&host.door),
    }
}

fn product_ids() -> HashSet<String> {
    PRODUCT_CATALOG.iter().map(|id| (*id).to_string()).collect()
}

/// Connect supervisor URLs into an orchestrator.
/// A blank primary stays unconfigured. A second URL does not fill that in.
pub(crate) fn connect(config: &DoorConfig) -> Result<Orchestrator, String> {
    let default_model = {
        let trimmed = config.default_model.trim();
        if trimmed.is_empty() {
            DEFAULT_CATALOG_ID.to_string()
        } else {
            trimmed.to_string()
        }
    };
    let primary = blank_to_none(config.primary_url.as_deref());
    let second = blank_to_none(config.second_url.as_deref());
    let Some(primary) = primary else {
        if !config.routes.is_empty() || !config.experts.is_empty() || !config.answers.is_empty() {
            return Err("a supervisor url is required before a route or an expert set".into());
        }
        return Ok(Orchestrator::unconfigured(&default_model));
    };
    if second.is_none() && (!config.routes.is_empty() || !config.experts.is_empty()) {
        return Err("a catalog route or an expert set needs a second supervisor url".into());
    }
    let first_name = crate::prompt::door_authority(&primary)?;
    let first = SupervisorDoor::connect(&primary)?;
    let Some(second) = second else {
        return Ok(Orchestrator::from_named(
            &default_model,
            vec![(first_name, Arc::new(first))],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ));
    };
    let second_name = crate::prompt::door_authority(&second)?;
    if first_name == second_name {
        return Err("supervisor urls must name different hosts".into());
    }
    let second_door = SupervisorDoor::connect(&second)?;
    let mut routes = Vec::new();
    for (model, url) in &config.routes {
        let name = crate::prompt::door_authority(url)?;
        if name != first_name && name != second_name {
            return Err(format!("route host {name} is not a configured supervisor"));
        }
        routes.push((model.clone(), name));
    }
    for (model, url) in &config.answers {
        if !config.experts.iter().any(|id| id.trim() == model.trim()) {
            return Err("an expert answer needs that catalog id marked as an expert set".into());
        }
        let name = crate::prompt::door_authority(url)?;
        if name != first_name && name != second_name {
            return Err(format!(
                "expert answer host {name} is not a configured supervisor"
            ));
        }
    }
    let answers = config
        .answers
        .iter()
        .map(|(model, url)| {
            let name = crate::prompt::door_authority(url).expect("checked");
            (model.clone(), name)
        })
        .collect();
    Ok(Orchestrator::from_named(
        &default_model,
        vec![
            (first_name, Arc::new(first)),
            (second_name, Arc::new(second_door)),
        ],
        routes,
        config.experts.clone(),
        answers,
    ))
}

fn blank_to_none(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// Two supervisor doors. `forward` calls both. Off unless both exist;
/// [`Orchestrator::plan`] is what turns a marked catalog id into this wrapper.
pub(crate) struct ExpertFan {
    first: Host,
    second: Host,
    answer_second: bool,
    report: Option<Arc<Report>>,
}

impl ExpertFan {
    #[cfg(test)]
    pub(crate) fn pair(
        first_name: &str,
        first: Arc<dyn PromptDoor>,
        second_name: &str,
        second: Arc<dyn PromptDoor>,
        answer_second: bool,
    ) -> Self {
        Self {
            first: Host {
                name: first_name.to_string(),
                door: first,
            },
            second: Host {
                name: second_name.to_string(),
                door: second,
            },
            answer_second,
            report: None,
        }
    }
}

impl PromptDoor for ExpertFan {
    fn forward(&self, call: &ForwardedPrompt) -> Result<String, String> {
        if call.api_key.trim().is_empty() {
            return Err("api key is required".into());
        }
        if let Some(report) = &self.report {
            *report
                .other
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = None;
        }
        let first = self.first.door.forward(call);
        let second = self.second.door.forward(call);
        match (first, second) {
            (Ok(left), Ok(right)) => {
                let (answer, other_name, other_text) = if self.answer_second {
                    (right, self.first.name.clone(), left)
                } else {
                    (left, self.second.name.clone(), right)
                };
                if let Some(report) = &self.report {
                    let answer_name = if self.answer_second {
                        self.second.name.clone()
                    } else {
                        self.first.name.clone()
                    };
                    report.set_door(&answer_name);
                    *report
                        .other
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner()) = Some(ExpertAside {
                        door: other_name,
                        response: other_text,
                    });
                }
                Ok(answer)
            }
            (Err(err), _) | (_, Err(err)) => Err(err),
        }
    }
}
