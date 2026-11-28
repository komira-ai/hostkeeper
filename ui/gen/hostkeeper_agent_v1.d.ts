// Generated from hostkeeper-proto's descriptors by hostkeeper-ui-tests (`client_gen`). Do not
// edit: `cargo test -p hostkeeper-ui-tests --test client` fails when this file differs from what
// the descriptors make, and writes the current text beside its test binary.
export namespace hostkeeper_agent_v1 {
  export interface AgentInfo { name?: string; version?: string; }
  export interface Baseline {
    memTotalKib?: string;
    memoryDevices?: hostkeeper_agent_v1.MemoryDevice[];
    cpusPresent?: number;
    cpusOnline?: number;
    smt?: string;
    disks?: hostkeeper_agent_v1.BaselineDisk[];
    nics?: hostkeeper_agent_v1.BaselineNic[];
  }
  export interface BaselineDisk { name?: string; sizeBytes?: string; serialDigest?: string; }
  export interface BaselineNic { name?: string; speedMbps?: number; }
  export interface BmcReport { ip?: string; mac?: string; }
  export interface Claims { machineId?: string; dmiSerial?: string; productUuid?: string; addresses?: string[]; macSerial?: string; platformUuid?: string; bmcMac?: string; }
  export interface CommandLineToolsState { receipt?: boolean; files?: boolean; version?: string; }
  export type CommitState = "COMMIT_STATE_UNSPECIFIED" | "COMMIT_STATE_UNCOMMITTED" | "COMMIT_STATE_COMMITTED";
  export interface DeferredReport { target?: string; reason?: string; retryAfterSecs?: string; }
  export interface DriftReport { found?: string; recorded?: string; }
  export interface DueItem { kind?: hostkeeper_agent_v1.MaintenanceKind; from?: string; to?: string; needsReboot?: boolean; label?: string; security?: boolean; }
  export type FailureClass = "FAILURE_CLASS_UNSPECIFIED" | "FAILURE_CLASS_RELEASE" | "FAILURE_CLASS_ENVIRONMENT";
  export type FailurePhase = "FAILURE_PHASE_UNSPECIFIED" | "FAILURE_PHASE_BEFORE_COMMIT" | "FAILURE_PHASE_GATE_RELEASE" | "FAILURE_PHASE_GATE_ENVIRONMENT" | "FAILURE_PHASE_AFTER_COMMIT" | "FAILURE_PHASE_HALT";
  export type FileVaultState = "FILE_VAULT_STATE_UNSPECIFIED" | "FILE_VAULT_STATE_OFF" | "FILE_VAULT_STATE_ON" | "FILE_VAULT_STATE_ENCRYPTING" | "FILE_VAULT_STATE_DECRYPTING";
  export type FullDiskAccess = "FULL_DISK_ACCESS_UNSPECIFIED" | "FULL_DISK_ACCESS_GRANTED" | "FULL_DISK_ACCESS_NOT_GRANTED" | "FULL_DISK_ACCESS_NOT_APPLICABLE" | "FULL_DISK_ACCESS_UNDETERMINED";
  export type HardwareComponent =
    | "HARDWARE_COMPONENT_UNSPECIFIED"
    | "HARDWARE_COMPONENT_MEMORY"
    | "HARDWARE_COMPONENT_CPU"
    | "HARDWARE_COMPONENT_PCIE"
    | "HARDWARE_COMPONENT_DISK"
    | "HARDWARE_COMPONENT_THERMAL"
    | "HARDWARE_COMPONENT_POWER"
    | "HARDWARE_COMPONENT_BOARD"
    | "HARDWARE_COMPONENT_HOST";
  export interface HardwareEvent {
    eventId?: string;
    timeUnix?: string;
    ofPreviousBoot?: boolean;
    component?: hostkeeper_agent_v1.HardwareComponent;
    subject?: string;
    severity?: hostkeeper_agent_v1.HardwareSeverity;
    state?: hostkeeper_agent_v1.HardwareEventState;
    source?: string;
    code?: string;
    message?: string;
    raw?: string;
    about?: string;
  }
  export type HardwareEventState = "HARDWARE_EVENT_STATE_UNSPECIFIED" | "HARDWARE_EVENT_STATE_EVENT" | "HARDWARE_EVENT_STATE_ASSERTED" | "HARDWARE_EVENT_STATE_DEASSERTED";
  export type HardwareSeverity = "HARDWARE_SEVERITY_UNSPECIFIED" | "HARDWARE_SEVERITY_INFO" | "HARDWARE_SEVERITY_WARNING" | "HARDWARE_SEVERITY_CRITICAL";
  export type Health = "HEALTH_UNSPECIFIED" | "HEALTH_HEALTHY" | "HEALTH_UNHEALTHY" | "HEALTH_ABSENT" | "HEALTH_SILENT";
  export interface HeartbeatRequest {
    platform?: hostkeeper_agent_v1.Platform;
    agent?: hostkeeper_agent_v1.AgentInfo;
    features?: string[];
    services?: hostkeeper_agent_v1.ServiceBeat[];
    sentUnix?: string;
    certNotAfterUnix?: string;
    lastRenewError?: string;
    trustedRootAgeSecs?: string;
    trustedRootReason?: hostkeeper_agent_v1.TrustedRootReason;
    installers?: string[];
    repositories?: hostkeeper_agent_v1.RepositoriesReport;
    factsGeneration?: string;
    remoteLoginProbe?: hostkeeper_agent_v1.RemoteLoginProbe;
    operatorKeys?: hostkeeper_agent_v1.OperatorKeysReport;
    prerequisites?: hostkeeper_agent_v1.PrerequisitesReport;
    ssh?: hostkeeper_agent_v1.SshReport;
    bmc?: hostkeeper_agent_v1.BmcReport;
    maintenance?: hostkeeper_agent_v1.MaintenanceReport;
    fullDiskAccess?: hostkeeper_agent_v1.FullDiskAccess;
    serialConsole?: hostkeeper_agent_v1.SerialConsoleReport;
    health?: hostkeeper_agent_v1.HostHealth;
  }
  export interface HostHealth {
    events?: hostkeeper_agent_v1.HardwareEvent[];
    baseline?: hostkeeper_agent_v1.Baseline;
    bootId?: string;
    bootUnix?: string;
    cleanPreviousShutdown?: boolean;
    sources?: hostkeeper_agent_v1.SourceState[];
  }
  export interface InstalledXcode { version?: string; build?: string; path?: string; selected?: boolean; foreign?: boolean; }
  export interface LocalHalt { reason?: string; generation?: string; ageSecs?: string; }
  export interface MacUpdateFacts {
    filevault?: hostkeeper_agent_v1.FileVaultState;
    authrestartSupported?: boolean;
    dataFreeBytes?: string;
    updateAccount?: hostkeeper_agent_v1.UpdateAccountState;
    commandLineTools?: hostkeeper_agent_v1.CommandLineToolsState;
    mdmConflicts?: string[];
  }
  export interface MaintenanceHost { osName?: string; osVersion?: string; osBuild?: string; hardwareSerial?: string; runningKernel?: string; newestKernel?: string; bootId?: string; bootUnix?: string; }
  export type MaintenanceKind =
    | "MAINTENANCE_KIND_UNSPECIFIED"
    | "MAINTENANCE_KIND_MACOS"
    | "MAINTENANCE_KIND_XCODE"
    | "MAINTENANCE_KIND_LINUX"
    | "MAINTENANCE_KIND_REBOOT"
    | "MAINTENANCE_KIND_PROBE"
    | "MAINTENANCE_KIND_REPAIR";
  export interface MaintenanceProbe { runId?: string; probedUnix?: string; steps?: hostkeeper_agent_v1.MaintenanceProbeStep[]; }
  export interface MaintenanceProbeStep { name?: string; exitCode?: number; output?: string; cut?: boolean; }
  export interface MaintenanceReport {
    host?: hostkeeper_agent_v1.MaintenanceHost;
    xcodes?: hostkeeper_agent_v1.InstalledXcode[];
    mac?: hostkeeper_agent_v1.MacUpdateFacts;
    due?: hostkeeper_agent_v1.DueItem[];
    dueLeftOut?: number;
    lookedUnix?: string;
    unparsedLines?: number;
    run?: hostkeeper_agent_v1.MaintenanceRunState;
    lookFailed?: string;
    probe?: hostkeeper_agent_v1.MaintenanceProbe;
    onDemand?: hostkeeper_agent_v1.MaintenanceKind[];
  }
  export interface MaintenanceRunState { runId?: string; orderId?: string; step?: hostkeeper_agent_v1.MaintenanceStep; applied?: hostkeeper_agent_v1.MaintenanceKind[]; haltedReason?: string; }
  export type MaintenanceStep =
    | "MAINTENANCE_STEP_UNSPECIFIED"
    | "MAINTENANCE_STEP_WAITING"
    | "MAINTENANCE_STEP_STAGING"
    | "MAINTENANCE_STEP_DRAINING"
    | "MAINTENANCE_STEP_APPLYING"
    | "MAINTENANCE_STEP_REBOOTING"
    | "MAINTENANCE_STEP_CHECKING"
    | "MAINTENANCE_STEP_PUTTING_BACK"
    | "MAINTENANCE_STEP_DONE"
    | "MAINTENANCE_STEP_DEFERRED"
    | "MAINTENANCE_STEP_HALTED";
  export interface MemoryDevice { locator?: string; sizeBytes?: string; }
  export interface OperatorKeysRefusal { digest?: string; reason?: string; }
  export interface OperatorKeysReport { digest?: string; refused?: hostkeeper_agent_v1.OperatorKeysRefusal; failed?: string; }
  export type Outcome = "OUTCOME_UNSPECIFIED" | "OUTCOME_SUCCEEDED" | "OUTCOME_FAILED";
  export interface PinReport { commit?: string; }
  export type Platform = "PLATFORM_UNSPECIFIED" | "PLATFORM_LINUX_X86_64" | "PLATFORM_LINUX_ARM64" | "PLATFORM_DARWIN_ARM64";
  export interface PrerequisiteState { name?: string; status?: hostkeeper_agent_v1.PrerequisiteStatus; version?: string; reason?: string; failures?: number; nextTryUnix?: string; seenUnix?: string; }
  export type PrerequisiteStatus =
    | "PREREQUISITE_STATUS_UNSPECIFIED"
    | "PREREQUISITE_STATUS_PENDING"
    | "PREREQUISITE_STATUS_PRESENT"
    | "PREREQUISITE_STATUS_INSTALLING"
    | "PREREQUISITE_STATUS_FAILED"
    | "PREREQUISITE_STATUS_NOT_APPLICABLE"
    | "PREREQUISITE_STATUS_UNKNOWN";
  export interface PrerequisitesRefusal { digest?: string; reason?: string; }
  export interface PrerequisitesReport { digest?: string; refused?: hostkeeper_agent_v1.PrerequisitesRefusal; states?: hostkeeper_agent_v1.PrerequisiteState[]; }
  export interface ProbeStep { name?: string; argv?: string[]; exitCode?: number; stdout?: string; stderr?: string; error?: string; truncated?: boolean; }
  export interface RemoteLoginProbe { startedUnix?: string; steps?: hostkeeper_agent_v1.ProbeStep[]; }
  export interface RepositoriesReport { generation?: string; digest?: string; refused?: hostkeeper_agent_v1.RepositoryRefusal; }
  export interface RepositoryRefusal { generation?: string; digest?: string; reason?: string; }
  export interface SerialConsoleReport { port?: string; state?: string; detail?: string; }
  export interface ServiceBeat {
    name?: string;
    version?: string;
    health?: hostkeeper_agent_v1.Health;
    lastUpdate?: hostkeeper_agent_v1.UpdateResult;
    updating?: string;
    step?: hostkeeper_agent_v1.StepRun;
    commit?: hostkeeper_agent_v1.CommitState;
    stepSecs?: { [key: string]: string };
    promotionPending?: boolean;
    deferred?: hostkeeper_agent_v1.DeferredReport;
    pinned?: hostkeeper_agent_v1.PinReport;
    recordTry?: string;
    installer?: string;
    drift?: hostkeeper_agent_v1.DriftReport;
    localHalt?: hostkeeper_agent_v1.LocalHalt;
    healthReason?: string;
    healthBody?: string;
    restarts?: hostkeeper_agent_v1.ServiceRestarts;
  }
  export interface ServiceRestarts { count?: number; recent?: number; lastResult?: string; exitStatus?: number; }
  export interface SourceState { source?: string; status?: hostkeeper_agent_v1.SourceStatus; reason?: string; }
  export type SourceStatus = "SOURCE_STATUS_UNSPECIFIED" | "SOURCE_STATUS_OK" | "SOURCE_STATUS_UNSUPPORTED" | "SOURCE_STATUS_UNREADABLE";
  export interface SshReport { appliedGeneration?: string; state?: string; hostKeyFingerprints?: string[]; detail?: string; }
  export interface StepDeferrals { count?: number; reason?: string; detail?: string; sinceUnix?: string; }
  export interface StepRun { name?: string; attempt?: number; deferrals?: hostkeeper_agent_v1.StepDeferrals; }
  export type TrustedRootReason = "TRUSTED_ROOT_REASON_UNSPECIFIED" | "TRUSTED_ROOT_REASON_NONE" | "TRUSTED_ROOT_REASON_STALE" | "TRUSTED_ROOT_REASON_MISSING" | "TRUSTED_ROOT_REASON_UNPARSEABLE";
  export interface UpdateAccountState { present?: boolean; admin?: boolean; secureToken?: boolean; hidden?: boolean; }
  export interface UpdateResult {
    target?: string;
    outcome?: hostkeeper_agent_v1.Outcome;
    phase?: hostkeeper_agent_v1.FailurePhase;
    class?: hostkeeper_agent_v1.FailureClass;
    reason?: string;
    startedUnix?: string;
    orderTry?: string;
    orderId?: string;
    forced?: boolean;
    detail?: string;
    logTail?: string[];
  }
}
