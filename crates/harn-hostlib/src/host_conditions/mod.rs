//! Portable, read-only ambient host-condition observations.
//!
//! Callers consume contention questions, never platform signal names. Local
//! probes translate the signals an environment exposes into a normalized
//! contention score, while injected sources let an orchestrator answer the
//! same contract from control-plane facts.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use harn_vm::VmValue;
use serde::{Deserialize, Serialize};
#[cfg(target_os = "linux")]
use std::path::Path;

use crate::error::HostlibError;
use crate::registry::{BuiltinRegistry, HostlibCapability, RegisteredBuiltin, SyncHandler};
use crate::tools::args::{build_dict, dict_arg, str_value};

/// Wire schema understood by this implementation.
pub const HOST_CONDITIONS_SCHEMA_VERSION: u32 = 1;

const SAMPLE_BUILTIN: &str = "hostlib_host_conditions_sample";

/// The stable questions a scheduler may ask about ambient contention.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostContentionQuestion {
    /// Whether the host is delivering the CPU capacity promised to this work.
    PromisedCpu,
    /// Whether execution is running below nominal speed.
    NominalSpeed,
    /// Whether an accelerator allocation is shared with other work.
    AcceleratorShared,
    /// Whether memory or storage pressure is delaying work.
    MemoryOrIoContended,
}

impl HostContentionQuestion {
    /// All questions in stable wire order.
    pub const ALL: [Self; 4] = [
        Self::PromisedCpu,
        Self::NominalSpeed,
        Self::AcceleratorShared,
        Self::MemoryOrIoContended,
    ];

    /// Stable wire spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PromisedCpu => "promised_cpu",
            Self::NominalSpeed => "nominal_speed",
            Self::AcceleratorShared => "accelerator_shared",
            Self::MemoryOrIoContended => "memory_or_io_contended",
        }
    }
}

/// The three observability states. Quiet is an `Observed` value of zero.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostConditionStatus {
    /// A real reading was obtained.
    Observed,
    /// The environment should expose an answer, but the read failed.
    Unavailable,
    /// This environment cannot expose an answer to the question.
    NotObservable,
}

impl HostConditionStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Unavailable => "unavailable",
            Self::NotObservable => "not_observable",
        }
    }
}

/// One environment-neutral answer to a contention question.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostConditionObservation {
    /// Question being answered.
    pub question: HostContentionQuestion,
    /// Whether the answer was observed, unavailable, or structurally hidden.
    pub status: HostConditionStatus,
    /// Normalized contention in `[0, 1]`; zero is a genuinely quiet reading.
    pub contention: Option<f64>,
    /// Stable explanation when no reading is available.
    pub reason: Option<String>,
}

impl HostConditionObservation {
    /// Construct a real normalized reading.
    pub fn observed(question: HostContentionQuestion, contention: f64) -> Self {
        Self {
            question,
            status: HostConditionStatus::Observed,
            contention: Some(contention.clamp(0.0, 1.0)),
            reason: None,
        }
    }

    /// Construct a transient read failure.
    pub fn unavailable(question: HostContentionQuestion, reason: impl Into<String>) -> Self {
        Self {
            question,
            status: HostConditionStatus::Unavailable,
            contention: None,
            reason: Some(reason.into()),
        }
    }

    /// Construct a structural observability limit.
    pub fn not_observable(question: HostContentionQuestion, reason: impl Into<String>) -> Self {
        Self {
            question,
            status: HostConditionStatus::NotObservable,
            contention: None,
            reason: Some(reason.into()),
        }
    }
}

/// Broad execution envelope used for attribution, not caller policy.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostEnvironment {
    /// A directly managed physical host.
    BareMetal,
    /// A hypervisor guest.
    Virtualized,
    /// A container or pod with cgroup-mediated resources.
    Containerized,
    /// The local backend cannot reliably classify the environment.
    Unknown,
}

impl HostEnvironment {
    const fn as_str(self) -> &'static str {
        match self {
            Self::BareMetal => "bare_metal",
            Self::Virtualized => "virtualized",
            Self::Containerized => "containerized",
            Self::Unknown => "unknown",
        }
    }
}

/// Versioned response shared by local probes and injected control-plane facts.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostConditionsSnapshot {
    /// Response schema version.
    pub schema_version: u32,
    /// Milliseconds since the Unix epoch when sampling completed.
    pub observed_at_ms: i64,
    /// Environment classification used by the selected source.
    pub environment: HostEnvironment,
    /// End-to-end source sampling cost in microseconds.
    pub sample_cost_us: u64,
    /// Exactly one answer for each stable contention question.
    pub questions: Vec<HostConditionObservation>,
}

impl HostConditionsSnapshot {
    fn normalize(&mut self) -> Result<(), String> {
        if self.schema_version != HOST_CONDITIONS_SCHEMA_VERSION {
            return Err(format!(
                "unsupported response schema_version {}; expected {}",
                self.schema_version, HOST_CONDITIONS_SCHEMA_VERSION
            ));
        }
        let mut by_question = BTreeMap::new();
        for observation in &self.questions {
            if by_question
                .insert(observation.question, observation)
                .is_some()
            {
                return Err(format!(
                    "duplicate answer for {}",
                    observation.question.as_str()
                ));
            }
            match observation.status {
                HostConditionStatus::Observed => {
                    let Some(value) = observation.contention else {
                        return Err(format!(
                            "{} is observed but has no contention value",
                            observation.question.as_str()
                        ));
                    };
                    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                        return Err(format!(
                            "{} contention must be finite and between 0 and 1",
                            observation.question.as_str()
                        ));
                    }
                    if observation.reason.is_some() {
                        return Err(format!(
                            "{} is observed but includes an absence reason",
                            observation.question.as_str()
                        ));
                    }
                }
                HostConditionStatus::Unavailable | HostConditionStatus::NotObservable => {
                    if observation.contention.is_some() {
                        return Err(format!(
                            "{} is not observed but includes a contention value",
                            observation.question.as_str()
                        ));
                    }
                    if observation
                        .reason
                        .as_deref()
                        .is_none_or(|reason| reason.trim().is_empty())
                    {
                        return Err(format!(
                            "{} is not observed but has no reason",
                            observation.question.as_str()
                        ));
                    }
                }
            }
        }
        for question in HostContentionQuestion::ALL {
            if !by_question.contains_key(&question) {
                return Err(format!("missing answer for {}", question.as_str()));
            }
        }
        drop(by_question);
        self.questions
            .sort_by_key(|observation| observation.question);
        Ok(())
    }
}

/// Pluggable owner of host-condition facts.
pub trait HostConditionsSource: Send + Sync + 'static {
    /// Sample one complete, versioned snapshot.
    fn sample(&self) -> Result<HostConditionsSnapshot, String>;
}

/// An injected immutable snapshot, useful for managed control-plane facts.
#[derive(Clone)]
pub struct InjectedHostConditionsSource {
    snapshot: HostConditionsSnapshot,
}

impl InjectedHostConditionsSource {
    /// Create an injected source. The capability boundary validates it before use.
    pub fn new(snapshot: HostConditionsSnapshot) -> Self {
        Self { snapshot }
    }
}

impl HostConditionsSource for InjectedHostConditionsSource {
    fn sample(&self) -> Result<HostConditionsSnapshot, String> {
        Ok(self.snapshot.clone())
    }
}

/// Cheap local probing based on the environment's native observability.
#[derive(Default)]
pub struct LocalHostConditionsSource;

impl HostConditionsSource for LocalHostConditionsSource {
    fn sample(&self) -> Result<HostConditionsSnapshot, String> {
        let started = Instant::now();
        let (environment, questions) = local_questions();
        Ok(HostConditionsSnapshot {
            schema_version: HOST_CONDITIONS_SCHEMA_VERSION,
            observed_at_ms: unix_time_ms(),
            environment,
            sample_cost_us: started.elapsed().as_micros().try_into().unwrap_or(u64::MAX),
            questions,
        })
    }
}

/// Hostlib bridge for ambient host conditions.
#[derive(Clone)]
pub struct HostConditionsCapability {
    source: Arc<dyn HostConditionsSource>,
}

impl Default for HostConditionsCapability {
    fn default() -> Self {
        Self::with_source(Arc::new(LocalHostConditionsSource))
    }
}

impl HostConditionsCapability {
    /// Install any source that produces the canonical snapshot contract.
    pub fn with_source(source: Arc<dyn HostConditionsSource>) -> Self {
        Self { source }
    }

    fn sample_builtin(&self, args: &[VmValue]) -> Result<VmValue, HostlibError> {
        let request = dict_arg(SAMPLE_BUILTIN, args)?;
        let version = match request.get("schema_version") {
            Some(VmValue::Int(version)) if *version > 0 => {
                u32::try_from(*version).map_err(|_| HostlibError::InvalidParameter {
                    builtin: SAMPLE_BUILTIN,
                    param: "schema_version",
                    message: "must fit in an unsigned 32-bit integer".to_string(),
                })?
            }
            None => {
                return Err(HostlibError::MissingParameter {
                    builtin: SAMPLE_BUILTIN,
                    param: "schema_version",
                });
            }
            _ => {
                return Err(HostlibError::InvalidParameter {
                    builtin: SAMPLE_BUILTIN,
                    param: "schema_version",
                    message: "must be a positive integer".to_string(),
                });
            }
        };
        if version != HOST_CONDITIONS_SCHEMA_VERSION {
            return Err(HostlibError::InvalidParameter {
                builtin: SAMPLE_BUILTIN,
                param: "schema_version",
                message: format!(
                    "unsupported version {version}; expected {HOST_CONDITIONS_SCHEMA_VERSION}"
                ),
            });
        }
        let mut snapshot = self
            .source
            .sample()
            .map_err(|message| HostlibError::Backend {
                builtin: SAMPLE_BUILTIN,
                message,
            })?;
        snapshot
            .normalize()
            .map_err(|message| HostlibError::Backend {
                builtin: SAMPLE_BUILTIN,
                message: format!("source returned an invalid host-conditions snapshot: {message}"),
            })?;
        snapshot_to_value(&snapshot)
    }
}

impl HostlibCapability for HostConditionsCapability {
    fn module_name(&self) -> &'static str {
        "host_conditions"
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
        let capability = self.clone();
        let handler: SyncHandler = Arc::new(move |args| capability.sample_builtin(args));
        registry.register(RegisteredBuiltin {
            name: SAMPLE_BUILTIN,
            module: "host_conditions",
            method: "sample",
            handler,
        });
    }
}

fn snapshot_to_value(snapshot: &HostConditionsSnapshot) -> Result<VmValue, HostlibError> {
    let questions = snapshot
        .questions
        .iter()
        .map(|observation| {
            build_dict([
                ("question", str_value(observation.question.as_str())),
                ("status", str_value(observation.status.as_str())),
                (
                    "contention",
                    observation
                        .contention
                        .map(VmValue::Float)
                        .unwrap_or(VmValue::Nil),
                ),
                (
                    "reason",
                    observation
                        .reason
                        .as_deref()
                        .map(str_value)
                        .unwrap_or(VmValue::Nil),
                ),
            ])
        })
        .collect();
    Ok(build_dict([
        (
            "schema_version",
            VmValue::Int(i64::from(snapshot.schema_version)),
        ),
        ("observed_at_ms", VmValue::Int(snapshot.observed_at_ms)),
        ("environment", str_value(snapshot.environment.as_str())),
        (
            "sample_cost_us",
            VmValue::Int(snapshot.sample_cost_us.try_into().map_err(|_| {
                HostlibError::Backend {
                    builtin: SAMPLE_BUILTIN,
                    message: "sample cost exceeds Harn integer range".to_string(),
                }
            })?),
        ),
        ("questions", VmValue::List(Arc::new(questions))),
    ]))
}

fn unix_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn local_questions() -> (HostEnvironment, Vec<HostConditionObservation>) {
    #[cfg(target_os = "linux")]
    {
        let input = LinuxProbeInput::read();
        linux_questions(&input)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let environment = local_non_linux_environment();
        #[cfg(target_os = "macos")]
        let promised_cpu = {
            let load = sysinfo::System::load_average().one;
            let cores = std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1);
            HostConditionObservation::observed(
                HostContentionQuestion::PromisedCpu,
                load / cores as f64,
            )
        };
        #[cfg(not(target_os = "macos"))]
        let promised_cpu = HostConditionObservation::unavailable(
            HostContentionQuestion::PromisedCpu,
            "native CPU contention counters are unavailable on this platform",
        );
        (
            environment,
            vec![
                promised_cpu,
                if environment == HostEnvironment::Virtualized {
                    HostConditionObservation::not_observable(
                        HostContentionQuestion::NominalSpeed,
                        "guest cannot observe hypervisor speed caps or host thermal throttling",
                    )
                } else {
                    HostConditionObservation::unavailable(
                        HostContentionQuestion::NominalSpeed,
                        "native nominal-speed probe is unavailable on this platform",
                    )
                },
                HostConditionObservation::not_observable(
                    HostContentionQuestion::AcceleratorShared,
                    "local allocation metadata does not expose accelerator sharing",
                ),
                HostConditionObservation::unavailable(
                    HostContentionQuestion::MemoryOrIoContended,
                    "native memory and IO pressure counters are unavailable on this platform",
                ),
            ],
        )
    }
}

#[cfg(target_os = "macos")]
fn local_non_linux_environment() -> HostEnvironment {
    let mut present: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>();
    let name = c"kern.hv_vmm_present";
    // SAFETY: `present` and `len` describe a valid writable integer buffer,
    // and the sysctl name is a static NUL-terminated string.
    let result = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&raw mut present).cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if result == 0 && present == 1 {
        HostEnvironment::Virtualized
    } else {
        HostEnvironment::BareMetal
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn local_non_linux_environment() -> HostEnvironment {
    HostEnvironment::Unknown
}

#[cfg(any(target_os = "linux", test))]
#[derive(Default)]
struct LinuxProbeInput {
    container_marker: bool,
    cgroup: Option<String>,
    product_name: Option<String>,
    proc_stat: Option<String>,
    cpu_stat: Option<String>,
    cpu_max: Option<String>,
    memory_pressure: Option<String>,
    io_pressure: Option<String>,
    load_one: f64,
    cores: usize,
    current_frequency_khz: Option<u64>,
    max_frequency_khz: Option<u64>,
}

#[cfg(any(target_os = "linux", test))]
impl LinuxProbeInput {
    #[cfg(target_os = "linux")]
    fn read() -> Self {
        Self {
            container_marker: Path::new("/.dockerenv").exists(),
            cgroup: read_to_string("/proc/1/cgroup"),
            product_name: read_to_string("/sys/class/dmi/id/product_name"),
            proc_stat: read_to_string("/proc/stat"),
            cpu_stat: read_to_string("/sys/fs/cgroup/cpu.stat"),
            cpu_max: read_to_string("/sys/fs/cgroup/cpu.max"),
            memory_pressure: read_to_string("/proc/pressure/memory"),
            io_pressure: read_to_string("/proc/pressure/io"),
            load_one: sysinfo::System::load_average().one,
            cores: std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1),
            current_frequency_khz: read_u64(
                "/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq",
            ),
            max_frequency_khz: read_u64("/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq"),
        }
    }
}

#[cfg(any(target_os = "linux", test))]
fn linux_questions(input: &LinuxProbeInput) -> (HostEnvironment, Vec<HostConditionObservation>) {
    let environment = classify_linux_environment(input);
    let promised_cpu = match environment {
        HostEnvironment::Containerized => parse_cgroup_throttle(input)
            .map(|value| {
                HostConditionObservation::observed(HostContentionQuestion::PromisedCpu, value)
            })
            .unwrap_or_else(|| {
                HostConditionObservation::unavailable(
                    HostContentionQuestion::PromisedCpu,
                    "container CPU quota or throttling counters could not be read",
                )
            }),
        HostEnvironment::Virtualized => parse_steal_fraction(input.proc_stat.as_deref())
            .map(|value| {
                HostConditionObservation::observed(HostContentionQuestion::PromisedCpu, value)
            })
            .unwrap_or_else(|| {
                HostConditionObservation::unavailable(
                    HostContentionQuestion::PromisedCpu,
                    "guest CPU steal counters could not be read",
                )
            }),
        HostEnvironment::BareMetal | HostEnvironment::Unknown => {
            HostConditionObservation::observed(
                HostContentionQuestion::PromisedCpu,
                input.load_one / input.cores.max(1) as f64,
            )
        }
    };
    let nominal_speed = match environment {
        HostEnvironment::Containerized => parse_cgroup_throttle(input)
            .map(|value| {
                HostConditionObservation::observed(HostContentionQuestion::NominalSpeed, value)
            })
            .unwrap_or_else(|| {
                HostConditionObservation::unavailable(
                    HostContentionQuestion::NominalSpeed,
                    "container CPU throttling counters could not be read",
                )
            }),
        HostEnvironment::Virtualized => HostConditionObservation::not_observable(
            HostContentionQuestion::NominalSpeed,
            "guest cannot observe host thermal throttling, credits, or hypervisor caps",
        ),
        HostEnvironment::BareMetal | HostEnvironment::Unknown => {
            match (input.current_frequency_khz, input.max_frequency_khz) {
                (Some(current), Some(max)) if max > 0 => HostConditionObservation::observed(
                    HostContentionQuestion::NominalSpeed,
                    1.0 - current as f64 / max as f64,
                ),
                _ => HostConditionObservation::unavailable(
                    HostContentionQuestion::NominalSpeed,
                    "CPU frequency counters could not be read",
                ),
            }
        }
    };
    let pressure = match (
        parse_psi(input.memory_pressure.as_deref()),
        parse_psi(input.io_pressure.as_deref()),
    ) {
        (Some(memory), Some(io)) => HostConditionObservation::observed(
            HostContentionQuestion::MemoryOrIoContended,
            memory.max(io),
        ),
        _ => HostConditionObservation::unavailable(
            HostContentionQuestion::MemoryOrIoContended,
            "memory or IO pressure counters could not be read",
        ),
    };
    (
        environment,
        vec![
            promised_cpu,
            nominal_speed,
            HostConditionObservation::not_observable(
                HostContentionQuestion::AcceleratorShared,
                "local allocation metadata does not expose accelerator sharing",
            ),
            pressure,
        ],
    )
}

#[cfg(any(target_os = "linux", test))]
fn classify_linux_environment(input: &LinuxProbeInput) -> HostEnvironment {
    let cgroup = input
        .cgroup
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if input.container_marker
        || ["docker", "containerd", "kubepods", "podman", "lxc"]
            .iter()
            .any(|needle| cgroup.contains(needle))
    {
        return HostEnvironment::Containerized;
    }
    let product = input
        .product_name
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if [
        "kvm",
        "qemu",
        "vmware",
        "virtualbox",
        "virtual machine",
        "xen",
        "amazon ec2",
        "google compute",
    ]
    .iter()
    .any(|needle| product.contains(needle))
    {
        HostEnvironment::Virtualized
    } else {
        HostEnvironment::BareMetal
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_cgroup_throttle(input: &LinuxProbeInput) -> Option<f64> {
    let stat = input.cpu_stat.as_deref()?;
    let quota = input.cpu_max.as_deref()?.split_whitespace().next()?;
    if quota == "max" {
        return None;
    }
    let fields: BTreeMap<_, _> = stat
        .lines()
        .filter_map(|line| line.split_once(' '))
        .collect();
    let usage: f64 = fields.get("usage_usec")?.parse().ok()?;
    let throttled: f64 = fields.get("throttled_usec")?.parse().ok()?;
    let total = usage + throttled;
    Some(if total > 0.0 { throttled / total } else { 0.0 })
}

#[cfg(any(target_os = "linux", test))]
fn parse_steal_fraction(proc_stat: Option<&str>) -> Option<f64> {
    let cpu = proc_stat?.lines().find(|line| line.starts_with("cpu "))?;
    let values: Vec<f64> = cpu
        .split_whitespace()
        .skip(1)
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    let steal = *values.get(7)?;
    let total: f64 = values.iter().sum();
    Some(if total > 0.0 { steal / total } else { 0.0 })
}

#[cfg(any(target_os = "linux", test))]
fn parse_psi(value: Option<&str>) -> Option<f64> {
    let some = value?.lines().find(|line| line.starts_with("some "))?;
    let avg10 = some
        .split_whitespace()
        .find_map(|field| field.strip_prefix("avg10="))?
        .parse::<f64>()
        .ok()?;
    Some(avg10 / 100.0)
}

#[cfg(target_os = "linux")]
fn read_to_string(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[cfg(target_os = "linux")]
fn read_u64(path: impl AsRef<Path>) -> Option<u64> {
    read_to_string(path)?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_uses_quota_throttling_and_pressure_questions() {
        let input = LinuxProbeInput {
            container_marker: true,
            cpu_stat: Some("usage_usec 900\nthrottled_usec 100\n".to_string()),
            cpu_max: Some("200000 100000\n".to_string()),
            memory_pressure: Some("some avg10=2.50 avg60=0.00 total=1\n".to_string()),
            io_pressure: Some("some avg10=4.00 avg60=0.00 total=1\n".to_string()),
            cores: 2,
            ..LinuxProbeInput::default()
        };
        let (environment, answers) = linux_questions(&input);
        assert_eq!(environment, HostEnvironment::Containerized);
        assert_eq!(answers[0].contention, Some(0.1));
        assert_eq!(answers[1].contention, Some(0.1));
        assert_eq!(answers[3].contention, Some(0.04));
    }

    #[test]
    fn virtual_guest_uses_steal_and_never_calls_thermal_absence_quiet() {
        let input = LinuxProbeInput {
            product_name: Some("KVM Virtual Machine".to_string()),
            proc_stat: Some("cpu  10 0 10 70 0 0 0 10 0 0\n".to_string()),
            memory_pressure: Some("some avg10=0.00 avg60=0.00 total=0\n".to_string()),
            io_pressure: Some("some avg10=0.00 avg60=0.00 total=0\n".to_string()),
            cores: 2,
            ..LinuxProbeInput::default()
        };
        let (environment, answers) = linux_questions(&input);
        assert_eq!(environment, HostEnvironment::Virtualized);
        assert_eq!(answers[0].contention, Some(0.1));
        assert_eq!(answers[1].status, HostConditionStatus::NotObservable);
        assert_eq!(answers[1].contention, None);
    }

    #[test]
    fn snapshot_rejects_missing_or_incoherent_answers() {
        let mut snapshot = HostConditionsSnapshot {
            schema_version: HOST_CONDITIONS_SCHEMA_VERSION,
            observed_at_ms: 1,
            environment: HostEnvironment::BareMetal,
            sample_cost_us: 10,
            questions: vec![HostConditionObservation {
                question: HostContentionQuestion::PromisedCpu,
                status: HostConditionStatus::Unavailable,
                contention: Some(0.0),
                reason: None,
            }],
        };
        assert!(snapshot.normalize().is_err());
    }
}
