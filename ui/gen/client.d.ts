// Generated from the protobuf descriptors of hostkeeper-proto by hostkeeper-ui-tests
// (`client_gen`). Do not edit: `cargo test -p hostkeeper-ui-tests --test client` fails when
// this file differs from what the descriptors make, and writes the current text beside its test
// binary.
//
// The types of `client.js`: each request and reply as proto3 JSON writes it. What each
// method does, and what each field holds, is said beside it in its `.proto` file under
// `crates/hostkeeper-proto/proto`.

/** Sends `request` to `method` (`<package>.<Service>/<Method>`) and resolves to the reply. */
export type Call = (method: string, request: object) => Promise<object>;

/** The operator API: one object per service, one function per method. */
export interface Client {
  readonly alerts: {
    list(request: hostkeeper_operator_v1.ListAlertsRequest): Promise<hostkeeper_operator_v1.ListAlertsResponse>;
  };
  readonly audit: {
    list(request: hostkeeper_operator_v1.ListAuditRequest): Promise<hostkeeper_operator_v1.ListAuditResponse>;
  };
  readonly enrollment: {
    list(request: hostkeeper_operator_v1.ListPendingRequest): Promise<hostkeeper_operator_v1.ListPendingResponse>;
    approve(request: hostkeeper_operator_v1.ApproveRequest): Promise<hostkeeper_operator_v1.ApproveResponse>;
    reject(request: hostkeeper_operator_v1.RejectRequest): Promise<hostkeeper_operator_v1.RejectResponse>;
    addRecord(request: hostkeeper_operator_v1.AddRecordRequest): Promise<hostkeeper_operator_v1.AddRecordResponse>;
    listRecords(request: hostkeeper_operator_v1.ListRecordsRequest): Promise<hostkeeper_operator_v1.ListRecordsResponse>;
    removeRecord(request: hostkeeper_operator_v1.RemoveRecordRequest): Promise<hostkeeper_operator_v1.RemoveRecordResponse>;
    openRecord(request: hostkeeper_operator_v1.OpenRecordRequest): Promise<hostkeeper_operator_v1.OpenRecordResponse>;
  };
  readonly hosts: {
    list(request: hostkeeper_operator_v1.ListHostsRequest): Promise<hostkeeper_operator_v1.ListHostsResponse>;
    get(request: hostkeeper_operator_v1.GetHostRequest): Promise<hostkeeper_operator_v1.GetHostResponse>;
    exclude(request: hostkeeper_operator_v1.ExcludeRequest): Promise<hostkeeper_operator_v1.ExcludeResponse>;
    include(request: hostkeeper_operator_v1.IncludeRequest): Promise<hostkeeper_operator_v1.IncludeResponse>;
    setLabels(request: hostkeeper_operator_v1.SetLabelsRequest): Promise<hostkeeper_operator_v1.SetLabelsResponse>;
    abort(request: hostkeeper_operator_v1.AbortRequest): Promise<hostkeeper_operator_v1.AbortResponse>;
    retire(request: hostkeeper_operator_v1.RetireRequest): Promise<hostkeeper_operator_v1.RetireResponse>;
  };
  readonly mdm: {
    listDevices(request: hostkeeper_operator_v1.ListDevicesRequest): Promise<hostkeeper_operator_v1.ListDevicesResponse>;
  };
  readonly releases: {
    setDesired(request: hostkeeper_operator_v1.SetDesiredRequest): Promise<hostkeeper_operator_v1.SetDesiredResponse>;
    converge(request: hostkeeper_operator_v1.ConvergeRequest): Promise<hostkeeper_operator_v1.ConvergeResponse>;
    force(request: hostkeeper_operator_v1.ForceRequest): Promise<hostkeeper_operator_v1.ForceResponse>;
    unmarkBad(request: hostkeeper_operator_v1.UnmarkBadRequest): Promise<hostkeeper_operator_v1.UnmarkBadResponse>;
    clearHalt(request: hostkeeper_operator_v1.ClearHaltRequest): Promise<hostkeeper_operator_v1.ClearHaltResponse>;
    status(request: hostkeeper_operator_v1.StatusRequest): Promise<hostkeeper_operator_v1.StatusResponse>;
    markBad(request: hostkeeper_operator_v1.MarkBadRequest): Promise<hostkeeper_operator_v1.MarkBadResponse>;
    halt(request: hostkeeper_operator_v1.HaltRequest): Promise<hostkeeper_operator_v1.HaltResponse>;
  };
  readonly rollouts: {
    advance(request: hostkeeper_operator_v1.AdvanceRequest): Promise<hostkeeper_operator_v1.AdvanceResponse>;
    pause(request: hostkeeper_operator_v1.PauseRequest): Promise<hostkeeper_operator_v1.PauseResponse>;
    resume(request: hostkeeper_operator_v1.ResumeRequest): Promise<hostkeeper_operator_v1.ResumeResponse>;
  };
  readonly services: {
    list(request: hostkeeper_operator_v1.ListServicesRequest): Promise<hostkeeper_operator_v1.ListServicesResponse>;
    assign(request: hostkeeper_operator_v1.AssignRequest): Promise<hostkeeper_operator_v1.AssignResponse>;
    unassign(request: hostkeeper_operator_v1.UnassignRequest): Promise<hostkeeper_operator_v1.UnassignResponse>;
    setMinVersion(request: hostkeeper_operator_v1.SetMinVersionRequest): Promise<hostkeeper_operator_v1.SetMinVersionResponse>;
    hold(request: hostkeeper_operator_v1.HoldRequest): Promise<hostkeeper_operator_v1.HoldResponse>;
    unhold(request: hostkeeper_operator_v1.UnholdRequest): Promise<hostkeeper_operator_v1.UnholdResponse>;
  };
}

export declare function createClient(call: Call): Client;

export namespace hostkeeper_agent_v1 {
  export interface AgentInfo {
    name?: string;
    version?: string;
  }
  export interface Claims {
    machineId?: string;
    dmiSerial?: string;
    productUuid?: string;
    addresses?: string[];
    macSerial?: string;
    platformUuid?: string;
    bmcMac?: string;
  }
  export type CommitState =
    | "COMMIT_STATE_UNSPECIFIED"
    | "COMMIT_STATE_UNCOMMITTED"
    | "COMMIT_STATE_COMMITTED";
  export interface DeferredReport {
    target?: string;
    reason?: string;
    retryAfterSecs?: string;
  }
  export type FailureClass =
    | "FAILURE_CLASS_UNSPECIFIED"
    | "FAILURE_CLASS_RELEASE"
    | "FAILURE_CLASS_ENVIRONMENT";
  export type FailurePhase =
    | "FAILURE_PHASE_UNSPECIFIED"
    | "FAILURE_PHASE_BEFORE_COMMIT"
    | "FAILURE_PHASE_GATE_RELEASE"
    | "FAILURE_PHASE_GATE_ENVIRONMENT"
    | "FAILURE_PHASE_AFTER_COMMIT"
    | "FAILURE_PHASE_HALT";
  export type Health =
    | "HEALTH_UNSPECIFIED"
    | "HEALTH_HEALTHY"
    | "HEALTH_UNHEALTHY"
    | "HEALTH_ABSENT";
  export interface HeartbeatRequest {
    platform?: hostkeeper_agent_v1.Platform;
    agent?: hostkeeper_agent_v1.AgentInfo;
    features?: string[];
    services?: hostkeeper_agent_v1.ServiceBeat[];
    sentUnix?: string;
    certNotAfterUnix?: string;
    lastRenewError?: string;
  }
  export type Outcome = "OUTCOME_UNSPECIFIED" | "OUTCOME_SUCCEEDED" | "OUTCOME_FAILED";
  export interface PinReport {
    commit?: string;
  }
  export type Platform =
    | "PLATFORM_UNSPECIFIED"
    | "PLATFORM_LINUX_X86_64"
    | "PLATFORM_LINUX_ARM64"
    | "PLATFORM_DARWIN_ARM64";
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
  }
  export interface StepRun {
    name?: string;
    attempt?: number;
  }
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

export namespace hostkeeper_operator_v1 {
  export interface AbortRequest {
    nodeId?: string;
    service?: string;
    reason?: string;
  }
  export interface AbortResponse {
    queued?: boolean;
    entry?: hostkeeper_operator_v1.AuditEntry;
  }
  export type Actor = "ACTOR_UNSPECIFIED" | "ACTOR_OPERATOR" | "ACTOR_CI" | "ACTOR_COORDINATOR";
  export interface AddRecordRequest {
    hostId?: string;
    role?: string;
    dataMacs?: string[];
    bmcMac?: string;
    serial?: string;
    notes?: string;
    approveUntilUnix?: string;
    approveForSecs?: string;
    platform?: hostkeeper_agent_v1.Platform;
  }
  export interface AddRecordResponse {
    record?: hostkeeper_operator_v1.InventoryRecord;
    entry?: hostkeeper_operator_v1.AuditEntry;
  }
  export interface AdvanceRequest {
    service?: string;
    reason?: string;
  }
  export interface AdvanceResponse {
    entry?: hostkeeper_operator_v1.AuditEntry;
    plan?: hostkeeper_operator_v1.RolloutPlan;
  }
  export interface Alert {
    kind?: hostkeeper_operator_v1.AlertKind;
    severity?: hostkeeper_operator_v1.AlertSeverity;
    hostId?: string;
    service?: string;
    version?: string;
    message?: string;
    firstObservedUnix?: string;
    requestId?: string;
    otherHostId?: string;
  }
  export type AlertKind =
    | "ALERT_KIND_UNSPECIFIED"
    | "ALERT_KIND_HALT"
    | "ALERT_KIND_WAITING_FOR_OPERATOR"
    | "ALERT_KIND_DUPLICATE"
    | "ALERT_KIND_DUPLICATE_CLAIM_PENDING"
    | "ALERT_KIND_FAILED_CANARY"
    | "ALERT_KIND_HELD_ON_BAD_VERSION"
    | "ALERT_KIND_ABANDONED_WHILE_UPDATING"
    | "ALERT_KIND_SILENT_HOST"
    | "ALERT_KIND_CERT_NEAR_EXPIRY"
    | "ALERT_KIND_STORE_UNREACHABLE";
  export type AlertSeverity =
    | "ALERT_SEVERITY_UNSPECIFIED"
    | "ALERT_SEVERITY_CRITICAL"
    | "ALERT_SEVERITY_WARNING";
  export interface ApproveRequest {
    requestId?: string;
    keySha256?: string;
    hostId?: string;
    role?: string;
    services?: string[];
    replace?: boolean;
    reason?: string;
  }
  export interface ApproveResponse {
    hostId?: string;
    certificateIssuedAtPoll?: boolean;
    entry?: hostkeeper_operator_v1.AuditEntry;
  }
  export interface AssignRequest {
    nodeId?: string;
    service?: string;
    role?: string;
  }
  export interface AssignResponse {
    entry?: hostkeeper_operator_v1.AuditEntry;
  }
  export interface AssignedHost {
    nodeId?: string;
    role?: string;
    installedVersion?: string;
    minVersion?: string;
    holds?: hostkeeper_operator_v1.HoldMark[];
  }
  export interface Assignment {
    role?: string;
    host?: string;
    removed?: boolean;
  }
  export interface AuditEntry {
    seq?: string;
    by?: hostkeeper_operator_v1.Actor;
    action?: string;
    service?: string;
    nodeId?: string;
    reason?: string;
    atUnix?: string;
    byName?: string;
  }
  export interface ClearHaltRequest {
    service?: string;
    reason?: string;
  }
  export interface ClearHaltResponse {
    entry?: hostkeeper_operator_v1.AuditEntry;
  }
  export interface ConvergeRequest {
    service?: string;
  }
  export interface ConvergeResponse {
    convergence?: hostkeeper_operator_v1.Convergence;
  }
  export interface Convergence {
    desired?: string;
    quiet?: boolean;
    state?: hostkeeper_operator_v1.ConvergenceState;
    roles?: hostkeeper_operator_v1.RoleCounts[];
    halt?: hostkeeper_operator_v1.HaltRecord;
    hosts?: hostkeeper_operator_v1.HostCounts;
    coordinatorCommit?: string;
    record?: hostkeeper_operator_v1.WriteRecord;
    force?: hostkeeper_operator_v1.ForceRecord;
  }
  export type ConvergenceState =
    | "CONVERGENCE_STATE_UNSPECIFIED"
    | "CONVERGENCE_STATE_CONVERGED"
    | "CONVERGENCE_STATE_BLOCKED"
    | "CONVERGENCE_STATE_HALTED"
    | "CONVERGENCE_STATE_IN_PROGRESS";
  export interface DesiredState {
    version?: string;
    maxUpdating?: { [key: string]: number };
    minUp?: { [key: string]: number };
    packageDigest?: string;
    releaseIndexDigest?: string;
  }
  export type EnrollmentRequestState =
    | "ENROLLMENT_REQUEST_STATE_UNSPECIFIED"
    | "ENROLLMENT_REQUEST_STATE_PENDING"
    | "ENROLLMENT_REQUEST_STATE_UNENROLLED";
  export interface ExcludeRequest {
    service?: string;
    nodeId?: string;
    reason?: string;
    abandon?: boolean;
  }
  export interface ExcludeResponse {
    entry?: hostkeeper_operator_v1.AuditEntry;
  }
  export interface Exclusion {
    service?: string;
    reason?: string;
  }
  export interface FailureAck {
    nodeId?: string;
    report?: hostkeeper_agent_v1.UpdateResult;
  }
  export interface FleetTarget {
    confirmService?: string;
  }
  export interface ForceRecord {
    version?: string;
    generation?: string;
    reason?: string;
  }
  export interface ForceRequest {
    service?: string;
    version?: string;
    reason?: string;
    fleet?: hostkeeper_operator_v1.FleetTarget;
    role?: string;
    hosts?: hostkeeper_operator_v1.HostList;
    includeHeld?: boolean;
  }
  export interface ForceResponse {
    entries?: hostkeeper_operator_v1.AuditEntry[];
    held?: string[];
    targeted?: number;
    skipped?: hostkeeper_operator_v1.SkippedHost[];
  }
  export type GateReason =
    | "GATE_REASON_UNSPECIFIED"
    | "GATE_REASON_NONE"
    | "GATE_REASON_HALTED"
    | "GATE_REASON_NO_DEFINITION"
    | "GATE_REASON_NO_DESIRED"
    | "GATE_REASON_NOT_A_MEMBER"
    | "GATE_REASON_QUIET"
    | "GATE_REASON_PINNED"
    | "GATE_REASON_ABSENT"
    | "GATE_REASON_UPDATING"
    | "GATE_REASON_BUSY_ELSEWHERE"
    | "GATE_REASON_UP_TO_DATE"
    | "GATE_REASON_FAILED_HERE"
    | "GATE_REASON_DEFERRED"
    | "GATE_REASON_NO_SLOT"
    | "GATE_REASON_WAITING_FOR_ROLE"
    | "GATE_REASON_WAITING_FOR_SERVICE"
    | "GATE_REASON_MIN_UP"
    | "GATE_REASON_BAKING"
    | "GATE_REASON_CANARY_WAIT"
    | "GATE_REASON_BAD"
    | "GATE_REASON_HELD"
    | "GATE_REASON_WAITING_FOR_OPERATOR"
    | "GATE_REASON_DUPLICATE"
    | "GATE_REASON_NO_PACKAGE_FOR_PLATFORM"
    | "GATE_REASON_PAUSED"
    | "GATE_REASON_ADVANCE_WAIT"
    | "GATE_REASON_STAGE_FAILED"
    | "GATE_REASON_STAGE_WAIT"
    | "GATE_REASON_BELOW_MIN_UP"
    | "GATE_REASON_ROLLOUT_ELSEWHERE";
  export interface GetHostRequest {
    nodeId?: string;
  }
  export interface GetHostResponse {
    host?: hostkeeper_operator_v1.Host;
  }
  export interface HaltRecord {
    nodeId?: string;
    check?: string;
    target?: string;
    report?: hostkeeper_agent_v1.UpdateResult;
  }
  export interface HaltRequest {
    service?: string;
    reason?: string;
  }
  export interface HaltResponse {
    entry?: hostkeeper_operator_v1.AuditEntry;
  }
  export interface HoldMark {
    scope?: hostkeeper_operator_v1.HoldScope;
    service?: string;
    reason?: string;
    setBy?: string;
    setUnix?: string;
  }
  export interface HoldRequest {
    scope?: hostkeeper_operator_v1.HoldScope;
    service?: string;
    reason?: string;
  }
  export interface HoldResponse {
    hold?: hostkeeper_operator_v1.HoldMark;
    entry?: hostkeeper_operator_v1.AuditEntry;
  }
  export interface HoldScope {
    host?: string;
    role?: string;
    fleet?: hostkeeper_operator_v1.WholeFleet;
  }
  export interface Host {
    nodeId?: string;
    role?: string;
    live?: boolean;
    lastHeartbeatUnix?: string;
    heartbeat?: hostkeeper_agent_v1.HeartbeatRequest;
    labels?: { [key: string]: string };
    exclusions?: hostkeeper_operator_v1.Exclusion[];
    services?: hostkeeper_operator_v1.HostService[];
    inventoryRecordId?: string;
    dataMacs?: string[];
    bmcMac?: string;
    serial?: string;
    enrolledUnix?: string;
    certNotAfterUnix?: string;
    lastRenewError?: string;
    clockSkewBeats?: string;
    firstHeartbeatUnix?: string;
    firstServingUnix?: string;
  }
  export interface HostCounts {
    missing?: number;
    absent?: number;
    excluded?: number;
    unmanaged?: number;
    deferred?: number;
  }
  export type HostHeld =
    | "HOST_HELD_UNSPECIFIED"
    | "HOST_HELD_NONE"
    | "HOST_HELD_APPROVED"
    | "HOST_HELD_DENIED"
    | "HOST_HELD_RETIRED";
  export interface HostList {
    nodeIds?: string[];
  }
  export interface HostService {
    service?: string;
    up?: boolean;
    holdsSlot?: boolean;
    gateReason?: hostkeeper_operator_v1.GateReason;
    membership?: hostkeeper_operator_v1.Membership;
    holds?: hostkeeper_operator_v1.HoldMark[];
    installedVersion?: string;
    health?: hostkeeper_agent_v1.Health;
    lastUpdate?: hostkeeper_agent_v1.UpdateResult;
    excludedReason?: string;
    stage?: number;
  }
  export interface IncludeRequest {
    service?: string;
    nodeId?: string;
  }
  export interface IncludeResponse {
    entry?: hostkeeper_operator_v1.AuditEntry;
  }
  export type InventoryMatch =
    | "INVENTORY_MATCH_UNSPECIFIED"
    | "INVENTORY_MATCH_MATCHED"
    | "INVENTORY_MATCH_UNMATCHED"
    | "INVENTORY_MATCH_AMBIGUOUS";
  export interface InventoryRecord {
    hostId?: string;
    role?: string;
    dataMacs?: string[];
    bmcMac?: string;
    serial?: string;
    notes?: string;
    addedBy?: string;
    addedUnix?: string;
    enrolled?: boolean;
    hostHeld?: hostkeeper_operator_v1.HostHeld;
    approveUntilUnix?: string;
    state?: hostkeeper_operator_v1.RecordState;
    usedByRequestId?: string;
    usedUnix?: string;
    platform?: hostkeeper_agent_v1.Platform;
  }
  export interface ListAlertsRequest {
  }
  export interface ListAlertsResponse {
    alerts?: hostkeeper_operator_v1.Alert[];
  }
  export interface ListAuditRequest {
    sinceSeq?: string;
    limit?: number;
    actor?: string;
    action?: string;
    service?: string;
    nodeId?: string;
    byName?: string;
    newest?: boolean;
  }
  export interface ListAuditResponse {
    entries?: hostkeeper_operator_v1.AuditEntry[];
    nextSeq?: string;
  }
  export interface ListDevicesRequest {
    limit?: number;
    pageToken?: string;
  }
  export interface ListDevicesResponse {
    devices?: hostkeeper_operator_v1.MdmDevice[];
    nextPageToken?: string;
    gate?: boolean;
  }
  export interface ListHostsRequest {
    limit?: number;
    pageToken?: string;
  }
  export interface ListHostsResponse {
    hosts?: hostkeeper_operator_v1.Host[];
    nextPageToken?: string;
  }
  export interface ListPendingRequest {
    limit?: number;
    pageToken?: string;
    unenrolled?: boolean;
  }
  export interface ListPendingResponse {
    requests?: hostkeeper_operator_v1.PendingEnrollment[];
    nextPageToken?: string;
  }
  export interface ListRecordsRequest {
    limit?: number;
    pageToken?: string;
    enrolled?: boolean;
  }
  export interface ListRecordsResponse {
    records?: hostkeeper_operator_v1.InventoryRecord[];
    nextPageToken?: string;
  }
  export interface ListServicesRequest {
  }
  export interface ListServicesResponse {
    services?: hostkeeper_operator_v1.ServiceEntry[];
    holds?: hostkeeper_operator_v1.HoldMark[];
  }
  export interface MarkBadRequest {
    service?: string;
    version?: string;
    reason?: string;
  }
  export interface MarkBadResponse {
    entry?: hostkeeper_operator_v1.AuditEntry;
  }
  export interface MdmCommand {
    requestType?: string;
    commandUuid?: string;
    sentUnix?: string;
  }
  export interface MdmDevice {
    enrollmentId?: string;
    udid?: string;
    serial?: string;
    authenticateSerial?: string;
    phase?: hostkeeper_operator_v1.MdmPhase;
    enrollments?: number;
    supervised?: boolean;
    enrolledViaDep?: boolean;
    userApproved?: boolean;
    enrolledSinceUnix?: string;
    lastCheckinUnix?: string;
    checkedOutUnix?: string;
    deviceInformationUnix?: string;
    securityInfoUnix?: string;
    unanswered?: hostkeeper_operator_v1.MdmCommand[];
    lastError?: string;
    installSends?: number;
    installLastSentUnix?: string;
    installSha256?: string;
  }
  export type MdmPhase =
    | "MDM_PHASE_UNSPECIFIED"
    | "MDM_PHASE_AUTHENTICATED"
    | "MDM_PHASE_ENROLLED"
    | "MDM_PHASE_CHECKED_OUT";
  export type Membership =
    | "MEMBERSHIP_UNSPECIFIED"
    | "MEMBERSHIP_PRESENT"
    | "MEMBERSHIP_EXCLUDED"
    | "MEMBERSHIP_NOT_SELECTED"
    | "MEMBERSHIP_DOWN"
    | "MEMBERSHIP_ABSENT"
    | "MEMBERSHIP_MISSING"
    | "MEMBERSHIP_UNMANAGED";
  export interface MinVersion {
    role?: string;
    host?: string;
    version?: string;
  }
  export interface OpenRecordRequest {
    hostId?: string;
    approveUntilUnix?: string;
    approveForSecs?: string;
    reason?: string;
  }
  export interface OpenRecordResponse {
    record?: hostkeeper_operator_v1.InventoryRecord;
    entry?: hostkeeper_operator_v1.AuditEntry;
  }
  export interface PauseRequest {
    service?: string;
    reason?: string;
  }
  export interface PauseResponse {
    entry?: hostkeeper_operator_v1.AuditEntry;
    plan?: hostkeeper_operator_v1.RolloutPlan;
  }
  export interface PendingEnrollment {
    requestId?: string;
    keySha256?: string;
    claims?: hostkeeper_agent_v1.Claims;
    image?: string;
    firstSeenUnix?: string;
    renewalOf?: string;
    duplicateClaim?: boolean;
    macNoncePresented?: boolean;
    inventoryMatch?: hostkeeper_operator_v1.InventoryMatch;
    recordHostId?: string;
    recordRole?: string;
    candidateHostIds?: string[];
    state?: hostkeeper_operator_v1.EnrollmentRequestState;
    unenrolledReason?: string;
    unenrolledUnix?: string;
    requestTtlSecs?: string;
    contestedBy?: string[];
    recordSerial?: string;
    recordBmcMac?: string;
    recordState?: hostkeeper_operator_v1.RecordState;
    recordApproveUntilUnix?: string;
  }
  export interface PlanHost {
    nodeId?: string;
    role?: string;
    state?: hostkeeper_operator_v1.PlanHostState;
    deferring?: boolean;
  }
  export type PlanHostState =
    | "PLAN_HOST_STATE_UNSPECIFIED"
    | "PLAN_HOST_STATE_WAITING"
    | "PLAN_HOST_STATE_UPDATING"
    | "PLAN_HOST_STATE_BAKING"
    | "PLAN_HOST_STATE_DONE"
    | "PLAN_HOST_STATE_FAILED";
  export interface PlanStage {
    number?: number;
    percent?: number;
    hosts?: number;
    advance?: hostkeeper_operator_v1.StageAdvance;
    members?: hostkeeper_operator_v1.PlanHost[];
  }
  export type PlanWait =
    | "PLAN_WAIT_UNSPECIFIED"
    | "PLAN_WAIT_NONE"
    | "PLAN_WAIT_HALTED"
    | "PLAN_WAIT_PAUSED"
    | "PLAN_WAIT_QUIET"
    | "PLAN_WAIT_ROLLOUT_ELSEWHERE"
    | "PLAN_WAIT_MANUAL_ADVANCE"
    | "PLAN_WAIT_FAILED_HOST"
    | "PLAN_WAIT_BELOW_MIN_UP"
    | "PLAN_WAIT_UPDATING"
    | "PLAN_WAIT_DEFERRING"
    | "PLAN_WAIT_BAKING";
  export type RecordState =
    | "RECORD_STATE_UNSPECIFIED"
    | "RECORD_STATE_OPEN"
    | "RECORD_STATE_CLOSED"
    | "RECORD_STATE_USED";
  export interface RejectRequest {
    requestId?: string;
    reason?: string;
  }
  export interface RejectResponse {
    entry?: hostkeeper_operator_v1.AuditEntry;
  }
  export interface RemoveRecordRequest {
    hostId?: string;
    reason?: string;
  }
  export interface RemoveRecordResponse {
    entry?: hostkeeper_operator_v1.AuditEntry;
  }
  export interface Replaced {
    version?: string;
    minUp?: { [key: string]: number };
  }
  export interface ResumeRequest {
    service?: string;
    reason?: string;
  }
  export interface ResumeResponse {
    entry?: hostkeeper_operator_v1.AuditEntry;
    plan?: hostkeeper_operator_v1.RolloutPlan;
  }
  export interface RetireRequest {
    nodeId?: string;
    reason?: string;
  }
  export interface RetireResponse {
    entry?: hostkeeper_operator_v1.AuditEntry;
    updateInFlight?: boolean;
  }
  export interface RoleCounts {
    role?: string;
    atDesired?: number;
    updating?: number;
    failed?: number;
    down?: number;
    pinned?: number;
  }
  export interface RolloutPlan {
    version?: string;
    stages?: hostkeeper_operator_v1.PlanStage[];
    current?: number;
    eligible?: number;
    waiting?: hostkeeper_operator_v1.PlanWait;
    paused?: boolean;
    pausedBy?: string;
    pauseReason?: string;
    finished?: boolean;
  }
  export interface ServiceEntry {
    service?: string;
    hosts?: string[];
    minVersions?: hostkeeper_operator_v1.MinVersion[];
    holds?: hostkeeper_operator_v1.HoldMark[];
    desiredVersion?: string;
    roles?: string[];
    assignments?: hostkeeper_operator_v1.Assignment[];
    assigned?: hostkeeper_operator_v1.AssignedHost[];
    uninstalling?: string[];
  }
  export interface ServiceHost {
    nodeId?: string;
    role?: string;
    live?: boolean;
    view?: hostkeeper_operator_v1.HostService;
    beat?: hostkeeper_agent_v1.ServiceBeat;
    excludedReason?: string;
  }
  export interface ServiceMarks {
    halt?: hostkeeper_operator_v1.HaltRecord;
    haltAcked?: hostkeeper_operator_v1.HaltRecord[];
    haltSeen?: hostkeeper_operator_v1.HaltRecord[];
    haltGeneration?: string;
    lastGood?: string;
    bad?: string[];
    badByForce?: string[];
    failureAcked?: hostkeeper_operator_v1.FailureAck[];
    forceGeneration?: string;
  }
  export interface ServiceStatus {
    service?: string;
    desired?: hostkeeper_operator_v1.DesiredState;
    convergence?: hostkeeper_operator_v1.Convergence;
    hosts?: hostkeeper_operator_v1.ServiceHost[];
    up?: number;
    slotsHeld?: number;
    marks?: hostkeeper_operator_v1.ServiceMarks;
    plan?: hostkeeper_operator_v1.RolloutPlan;
  }
  export interface SetDesiredRequest {
    service?: string;
    desired?: hostkeeper_operator_v1.DesiredState;
    ifGeneration?: string;
    writeId?: string;
    packageDigest?: string;
    releaseIndex?: string;
    releaseIndexDigest?: string;
  }
  export interface SetDesiredResponse {
    desired?: hostkeeper_operator_v1.DesiredState;
    record?: hostkeeper_operator_v1.WriteRecord;
    droppedRoles?: string[];
  }
  export interface SetLabelsRequest {
    nodeId?: string;
    labels?: { [key: string]: string };
  }
  export interface SetLabelsResponse {
    entry?: hostkeeper_operator_v1.AuditEntry;
  }
  export interface SetMinVersionRequest {
    service?: string;
    minVersion?: hostkeeper_operator_v1.MinVersion;
  }
  export interface SetMinVersionResponse {
    entry?: hostkeeper_operator_v1.AuditEntry;
  }
  export interface SkippedHost {
    nodeId?: string;
    reason?: string;
  }
  export type StageAdvance =
    | "STAGE_ADVANCE_UNSPECIFIED"
    | "STAGE_ADVANCE_AUTO"
    | "STAGE_ADVANCE_MANUAL";
  export interface StatusRequest {
    service?: string;
  }
  export interface StatusResponse {
    services?: hostkeeper_operator_v1.ServiceStatus[];
    quiet?: boolean;
    servicesDropped?: string;
    factsDropped?: { [key: string]: string };
    audit?: hostkeeper_operator_v1.AuditEntry[];
  }
  export interface UnassignRequest {
    nodeId?: string;
    service?: string;
    uninstall?: boolean;
    role?: string;
  }
  export interface UnassignResponse {
    entry?: hostkeeper_operator_v1.AuditEntry;
  }
  export interface UnholdRequest {
    scope?: hostkeeper_operator_v1.HoldScope;
    service?: string;
    reason?: string;
  }
  export interface UnholdResponse {
    entry?: hostkeeper_operator_v1.AuditEntry;
  }
  export interface UnmarkBadRequest {
    service?: string;
    version?: string;
    reason?: string;
  }
  export interface UnmarkBadResponse {
    entry?: hostkeeper_operator_v1.AuditEntry;
  }
  export interface WholeFleet {
  }
  export interface WriteRecord {
    generation?: string;
    replaced?: hostkeeper_operator_v1.Replaced;
    writeId?: string;
  }
}
