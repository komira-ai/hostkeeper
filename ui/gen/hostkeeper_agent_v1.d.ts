// Generated from hostkeeper-proto's descriptors by hostkeeper-ui-tests (`client_gen`). Do not
// edit: `cargo test -p hostkeeper-ui-tests --test client` fails when this file differs from what
// the descriptors make, and writes the current text beside its test binary.
export namespace hostkeeper_agent_v1 {
  export interface AgentInfo { name?: string; version?: string; }
  export interface Claims { machineId?: string; dmiSerial?: string; productUuid?: string; addresses?: string[]; macSerial?: string; platformUuid?: string; bmcMac?: string; }
  export type CommitState = "COMMIT_STATE_UNSPECIFIED" | "COMMIT_STATE_UNCOMMITTED" | "COMMIT_STATE_COMMITTED";
  export interface DeferredReport { target?: string; reason?: string; retryAfterSecs?: string; }
  export interface DriftReport { found?: string; recorded?: string; }
  export type FailureClass = "FAILURE_CLASS_UNSPECIFIED" | "FAILURE_CLASS_RELEASE" | "FAILURE_CLASS_ENVIRONMENT";
  export type FailurePhase = "FAILURE_PHASE_UNSPECIFIED" | "FAILURE_PHASE_BEFORE_COMMIT" | "FAILURE_PHASE_GATE_RELEASE" | "FAILURE_PHASE_GATE_ENVIRONMENT" | "FAILURE_PHASE_AFTER_COMMIT" | "FAILURE_PHASE_HALT";
  export type Health = "HEALTH_UNSPECIFIED" | "HEALTH_HEALTHY" | "HEALTH_UNHEALTHY" | "HEALTH_ABSENT";
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
    serviceIdentities?: hostkeeper_agent_v1.ServiceIdentityReport[];
  }
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
  }
  export interface ServiceIdentityReport { service?: string; notAfterUnix?: string; lastError?: string; }
  export interface SshReport { appliedGeneration?: string; state?: string; hostKeyFingerprints?: string[]; detail?: string; }
  export interface StepRun { name?: string; attempt?: number; }
  export type TrustedRootReason = "TRUSTED_ROOT_REASON_UNSPECIFIED" | "TRUSTED_ROOT_REASON_NONE" | "TRUSTED_ROOT_REASON_STALE" | "TRUSTED_ROOT_REASON_MISSING" | "TRUSTED_ROOT_REASON_UNPARSEABLE";
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
