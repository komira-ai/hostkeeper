// Generated from hostkeeper-proto's descriptors by hostkeeper-ui-tests (`client_gen`). Do not
// edit: `cargo test -p hostkeeper-ui-tests --test client` fails when this file differs from what
// the descriptors make, and writes the current text beside its test binary.
import type { hostkeeper_agent_v1 } from "./hostkeeper_agent_v1.js";
import type { hostkeeper_release_v1 } from "./hostkeeper_release_v1.js";

export namespace hostkeeper_operator_v1 {
  export interface AbortRequest { nodeId?: string; service?: string; reason?: string; }
  export interface AbortResponse { queued?: boolean; entry?: hostkeeper_operator_v1.AuditEntry; }
  export type Actor = "ACTOR_UNSPECIFIED" | "ACTOR_OPERATOR" | "ACTOR_CI" | "ACTOR_COORDINATOR";
  export interface AddLayoutRowRequest { reason?: string; }
  export interface AddLayoutRowResponse { row?: hostkeeper_operator_v1.LayoutRow; entry?: hostkeeper_operator_v1.AuditEntry; }
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
    hostGroup?: string;
  }
  export interface AddRecordResponse { record?: hostkeeper_operator_v1.InventoryRecord; entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface AddRepositoryRequest { name?: string; baseUri?: string; access?: string; credentialFile?: string; descriptorSha256?: string; fingerprints?: string[]; reason?: string; }
  export interface AddRepositoryResponse { repository?: hostkeeper_operator_v1.RegisteredRepository; entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface AdvanceRequest { service?: string; reason?: string; hostGroup?: string; }
  export interface AdvanceResponse { entry?: hostkeeper_operator_v1.AuditEntry; plan?: hostkeeper_operator_v1.RolloutPlan; }
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
    hostGroup?: string;
    reason?: string;
    detail?: string;
    tries?: string;
    sinceUnix?: string;
    lastSeenUnix?: string;
    acknowledged?: boolean;
    likelyCause?: string;
    events?: string[];
    finding?: string;
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
    | "ALERT_KIND_STORE_UNREACHABLE"
    | "ALERT_KIND_TRUSTED_ROOT"
    | "ALERT_KIND_PUSH_CERT_NEAR_EXPIRY"
    | "ALERT_KIND_MDM_TLS_RENEWAL_FAILED"
    | "ALERT_KIND_MDM_DNS_CREDENTIAL_REJECTED"
    | "ALERT_KIND_MDM_IDENTITY_NEAR_EXPIRY"
    | "ALERT_KIND_PREREQUISITE_FAILED"
    | "ALERT_KIND_XCODE_RELEASE_WAITING"
    | "ALERT_KIND_XCODE_VERSION_TAKEN"
    | "ALERT_KIND_UPDATES_DUE"
    | "ALERT_KIND_FAILED_UPDATE"
    | "ALERT_KIND_HOST_HALT"
    | "ALERT_KIND_MAINTENANCE_HALTED"
    | "ALERT_KIND_MAINTENANCE_NOT_BACK"
    | "ALERT_KIND_REPEATED_DEFERRAL"
    | "ALERT_KIND_HARDWARE_FAULT"
    | "ALERT_KIND_CAPACITY_REGRESSED"
    | "ALERT_KIND_STORAGE_FAILING"
    | "ALERT_KIND_UNEXPECTED_REBOOT"
    | "ALERT_KIND_SERVICE_UNHEALTHY"
    | "ALERT_KIND_SERVICE_CRASH_LOOP"
    | "ALERT_KIND_HOST_NOT_MONITORED"
    | "ALERT_KIND_BMC_MISSING"
    | "ALERT_KIND_SILENT_WHILE_UPDATING";
  export type AlertSeverity = "ALERT_SEVERITY_UNSPECIFIED" | "ALERT_SEVERITY_CRITICAL" | "ALERT_SEVERITY_WARNING";
  export interface ApiTokenInfo { name?: string; source?: hostkeeper_operator_v1.ApiTokenSource; createdBy?: string; createdUnix?: string; owner?: string; }
  export type ApiTokenSource = "API_TOKEN_SOURCE_UNSPECIFIED" | "API_TOKEN_SOURCE_STORE" | "API_TOKEN_SOURCE_TOKENS_FILE";
  export interface ApproveRequest {
    requestId?: string;
    keySha256?: string;
    hostId?: string;
    role?: string;
    services?: string[];
    replace?: boolean;
    reason?: string;
    hostGroup?: string;
    rack?: string;
    placementU?: number;
    placementHeightU?: number;
    placementSled?: string;
    placementSlot?: number;
  }
  export interface ApproveResponse { hostId?: string; certificateIssuedAtPoll?: boolean; entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface AssignRequest { nodeId?: string; service?: string; role?: string; }
  export interface AssignResponse { entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface AssignedHost { nodeId?: string; role?: string; installedVersion?: string; minVersion?: string; holds?: hostkeeper_operator_v1.HoldMark[]; }
  export interface Assignment { role?: string; host?: string; removed?: boolean; }
  export interface AuditEntry {
    seq?: string;
    by?: hostkeeper_operator_v1.Actor;
    action?: string;
    service?: string;
    nodeId?: string;
    reason?: string;
    atUnix?: string;
    byName?: string;
    hostGroup?: string;
    apiToken?: string;
  }
  export type BootDevice = "BOOT_DEVICE_UNSPECIFIED" | "BOOT_DEVICE_PXE" | "BOOT_DEVICE_DISK" | "BOOT_DEVICE_BIOS";
  export interface Canary { count?: number; }
  export interface CancelMaintenanceRunRequest { runId?: string; reason?: string; }
  export interface CancelMaintenanceRunResponse { run?: hostkeeper_operator_v1.MaintenanceRun; entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface ClearHaltRequest { service?: string; reason?: string; }
  export interface ClearHaltResponse { entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface ClearRecordRequest { nodeId?: string; service?: string; }
  export interface ClearRecordResponse { orderId?: string; }
  export interface CloseConsoleRequest { hostId?: string; }
  export interface CloseConsoleResponse {}
  export interface ConfirmEnrollmentRequest { serial?: string; }
  export interface ConfirmEnrollmentResponse { serial?: string; expiresUnix?: string; }
  export interface ConsoleAddress { address?: string; source?: hostkeeper_operator_v1.ConsoleAddressSource; stale?: boolean; reportedUnix?: string; }
  export type ConsoleAddressSource = "CONSOLE_ADDRESS_SOURCE_UNSPECIFIED" | "CONSOLE_ADDRESS_SOURCE_TARGETS_FILE" | "CONSOLE_ADDRESS_SOURCE_HEARTBEAT" | "CONSOLE_ADDRESS_SOURCE_NEIGHBOR_TABLE";
  export type ConsoleKind = "CONSOLE_KIND_UNSPECIFIED" | "CONSOLE_KIND_SERIAL" | "CONSOLE_KIND_SCREEN";
  export interface ConsolePowerRequest { hostId?: string; action?: hostkeeper_operator_v1.PowerAction; reason?: string; }
  export interface ConsolePowerResponse { state?: hostkeeper_operator_v1.PowerState; sent?: hostkeeper_operator_v1.PowerAction; address?: hostkeeper_operator_v1.ConsoleAddress; }
  export type ConsoleProbeClass =
    | "CONSOLE_PROBE_CLASS_UNSPECIFIED"
    | "CONSOLE_PROBE_CLASS_SILENT"
    | "CONSOLE_PROBE_CLASS_LOGIN_PROMPT"
    | "CONSOLE_PROBE_CLASS_KERNEL_OUTPUT"
    | "CONSOLE_PROBE_CLASS_FIRMWARE_OUTPUT"
    | "CONSOLE_PROBE_CLASS_OTHER_OUTPUT";
  export type ConsoleProbeEnd = "CONSOLE_PROBE_END_UNSPECIFIED" | "CONSOLE_PROBE_END_MAX_BYTES" | "CONSOLE_PROBE_END_QUIET" | "CONSOLE_PROBE_END_WAIT" | "CONSOLE_PROBE_END_STREAM_ENDED";
  export interface ConsoleProbeFinding { class?: hostkeeper_operator_v1.ConsoleProbeClass; evidence?: string; }
  export interface ConsoleSession { hostId?: string; kind?: hostkeeper_operator_v1.ConsoleKind; openedBy?: string; openedUnix?: string; attached?: boolean; reason?: string; }
  export interface ConvergeRequest { service?: string; hostGroup?: string; }
  export interface ConvergeResponse { convergence?: hostkeeper_operator_v1.Convergence; }
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
    installerDropped?: string[];
  }
  export type ConvergenceState = "CONVERGENCE_STATE_UNSPECIFIED" | "CONVERGENCE_STATE_CONVERGED" | "CONVERGENCE_STATE_BLOCKED" | "CONVERGENCE_STATE_HALTED" | "CONVERGENCE_STATE_IN_PROGRESS";
  export interface CreateApiTokenRequest { name?: string; owner?: string; }
  export interface CreateApiTokenResponse { token?: hostkeeper_operator_v1.ApiTokenInfo; secret?: string; }
  export interface CreatePushRequestRequest { email?: string; submit?: boolean; }
  export interface CreatePushRequestResponse { csrPem?: string; encryptionCertificatePem?: string; submitted?: boolean; }
  export interface CreateUserRequest { name?: string; }
  export interface CreateUserResponse { user?: hostkeeper_operator_v1.UserInfo; password?: string; }
  export interface CurrentOrder { orderId?: string; orderTry?: string; version?: string; uninstall?: boolean; }
  export interface DeferBackoff { initialSecs?: string; factor?: number; maxSecs?: string; }
  export interface DeleteUserRequest { name?: string; }
  export interface DeleteUserResponse {}
  export interface DesiredState { version?: string; maxUpdating?: { [key: string]: number }; minUp?: { [key: string]: number }; packageDigest?: string; releaseIndexDigest?: string; }
  export interface DisableUserRequest { name?: string; }
  export interface DisableUserResponse { user?: hostkeeper_operator_v1.UserInfo; }
  export interface DiscoverRepositoryRequest { baseUri?: string; access?: string; credentialFile?: string; }
  export interface DiscoverRepositoryResponse { discovery?: hostkeeper_operator_v1.Discovery; }
  export interface Discovery {
    name?: string;
    description?: string;
    channels?: string[];
    platforms?: string[];
    macosPackages?: string[];
    fingerprints?: string[];
    userIds?: string[];
    descriptorSha256?: string;
    keyringSha256?: string;
    pinned?: boolean;
  }
  export interface EnableUserRequest { name?: string; }
  export interface EnableUserResponse { user?: hostkeeper_operator_v1.UserInfo; }
  export interface EnrollmentRequest { serial?: string; udid?: string; product?: string; reason?: string; firstUnix?: string; lastUnix?: string; count?: number; inAbm?: boolean; confirmedUntilUnix?: string; }
  export type EnrollmentRequestState = "ENROLLMENT_REQUEST_STATE_UNSPECIFIED" | "ENROLLMENT_REQUEST_STATE_PENDING" | "ENROLLMENT_REQUEST_STATE_UNENROLLED";
  export interface ExcludeRequest { service?: string; nodeId?: string; reason?: string; abandon?: boolean; }
  export interface ExcludeResponse { entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface Exclusion { service?: string; reason?: string; }
  export interface FailThreshold { percent?: number; minHosts?: number; }
  export type FailedHosts = "FAILED_HOSTS_UNSPECIFIED" | "FAILED_HOSTS_STOP" | "FAILED_HOSTS_CONTINUE";
  export interface FailureAck { nodeId?: string; report?: hostkeeper_agent_v1.UpdateResult; }
  export interface FleetTarget { confirmService?: string; }
  export interface ForceRecord { version?: string; generation?: string; reason?: string; }
  export interface ForceRequest {
    service?: string;
    version?: string;
    reason?: string;
    fleet?: hostkeeper_operator_v1.FleetTarget;
    role?: string;
    hosts?: hostkeeper_operator_v1.HostList;
    includeHeld?: boolean;
    hostGroup?: string;
  }
  export interface ForceResponse { entries?: hostkeeper_operator_v1.AuditEntry[]; held?: string[]; targeted?: number; skipped?: hostkeeper_operator_v1.SkippedHost[]; }
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
    | "GATE_REASON_ROLLOUT_ELSEWHERE"
    | "GATE_REASON_NO_TARGET"
    | "GATE_REASON_NO_BUILD_FOR_HOST"
    | "GATE_REASON_INSTALLER_DROPPED"
    | "GATE_REASON_INSTALL_ORDER"
    | "GATE_REASON_MOVING"
    | "GATE_REASON_UNINSTALL_ORDER"
    | "GATE_REASON_HALTED_HERE"
    | "GATE_REASON_STALE_HALT_HERE";
  export interface GetAbmPublicKeyRequest {}
  export interface GetAbmPublicKeyResponse { certificatePem?: string; fingerprintSha256?: string; created?: boolean; }
  export interface GetHostGroupRequest { hostGroup?: string; limit?: number; pageToken?: string; }
  export interface GetHostGroupResponse { hostGroup?: hostkeeper_operator_v1.HostGroupState; nodeIds?: string[]; nextPageToken?: string; }
  export interface GetHostRequest { nodeId?: string; }
  export interface GetHostResponse { host?: hostkeeper_operator_v1.Host; }
  export interface GetLayoutRequest {}
  export interface GetLayoutResponse {
    rows?: hostkeeper_operator_v1.LayoutRow[];
    racks?: hostkeeper_operator_v1.LayoutRack[];
    placements?: hostkeeper_operator_v1.LayoutPlacement[];
    name?: string;
    devices?: hostkeeper_operator_v1.NetworkDevice[];
    sleds?: hostkeeper_operator_v1.LayoutSled[];
  }
  export interface GetMaintenanceRunRequest { runId?: string; }
  export interface GetMaintenanceRunResponse { run?: hostkeeper_operator_v1.MaintenanceRun; hosts?: hostkeeper_operator_v1.MaintenanceHost[]; }
  export interface GetReleaseRequest { service?: string; version?: string; hostGroup?: string; }
  export interface GetReleaseResponse { targets?: { [key: string]: hostkeeper_release_v1.TargetList }; releaseIndexDigest?: string; }
  export interface GetRepositoryRequest { name?: string; }
  export interface GetRepositoryResponse { repository?: hostkeeper_operator_v1.RegisteredRepository; }
  export interface GroupPackage { package?: string; desiredVersion?: string; minVersion?: string; convergence?: hostkeeper_operator_v1.Convergence; }
  export interface HaltRecord { nodeId?: string; check?: string; target?: string; report?: hostkeeper_agent_v1.UpdateResult; }
  export interface HaltRequest { service?: string; reason?: string; }
  export interface HaltResponse { entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface HoldMark { scope?: hostkeeper_operator_v1.HoldScope; service?: string; reason?: string; setBy?: string; setUnix?: string; }
  export interface HoldRequest { scope?: hostkeeper_operator_v1.HoldScope; service?: string; reason?: string; }
  export interface HoldResponse { hold?: hostkeeper_operator_v1.HoldMark; entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface HoldScope { host?: string; role?: string; fleet?: hostkeeper_operator_v1.WholeFleet; }
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
    trustedRootAgeSecs?: string;
    trustedRootReason?: hostkeeper_agent_v1.TrustedRootReason;
    installers?: string[];
    running?: hostkeeper_operator_v1.RunningService[];
    runningUnix?: string;
    repositories?: hostkeeper_operator_v1.HostRepositories;
    hostGroup?: string;
    maintenance?: hostkeeper_agent_v1.MaintenanceReport;
    maintenanceLookAgeSecs?: string;
    maintenanceStale?: boolean;
    fullDiskAccess?: hostkeeper_agent_v1.FullDiskAccess;
  }
  export interface HostCounts { missing?: number; absent?: number; excluded?: number; unmanaged?: number; deferred?: number; picked?: number; noTarget?: number; noBuildForHost?: number; installerDropped?: number; }
  export interface HostGroup { name?: string; packages?: string[]; policy?: hostkeeper_operator_v1.RolloutPolicy; }
  export interface HostGroupState { group?: hostkeeper_operator_v1.HostGroup; generation?: string; hosts?: number; packages?: hostkeeper_operator_v1.GroupPackage[]; }
  export type HostHeld = "HOST_HELD_UNSPECIFIED" | "HOST_HELD_NONE" | "HOST_HELD_APPROVED" | "HOST_HELD_DENIED" | "HOST_HELD_RETIRED";
  export interface HostKeysRequest { hostId?: string; }
  export interface HostKeysResponse { fingerprints?: string[]; state?: string; }
  export interface HostList { nodeIds?: string[]; }
  export interface HostRepositories { named?: string[]; unregistered?: string[]; digest?: string; generation?: string; current?: boolean; refused?: string; }
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
    order?: hostkeeper_operator_v1.CurrentOrder;
  }
  export interface IncludeRequest { service?: string; nodeId?: string; }
  export interface IncludeResponse { entry?: hostkeeper_operator_v1.AuditEntry; }
  export type InventoryMatch = "INVENTORY_MATCH_UNSPECIFIED" | "INVENTORY_MATCH_MATCHED" | "INVENTORY_MATCH_UNMATCHED" | "INVENTORY_MATCH_AMBIGUOUS";
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
    hostGroup?: string;
    enrolledHostId?: string;
    installedMemoryBytes?: string;
    memoryModules?: number;
  }
  export interface LayoutPlacement { subject?: hostkeeper_operator_v1.LayoutSubject; rack?: string; heightU?: number; u?: number; sled?: string; slot?: number; address?: string; }
  export interface LayoutRack { id?: string; row?: string; heightU?: number; number?: number; }
  export interface LayoutRow { id?: string; number?: number; }
  export interface LayoutSled { id?: string; rack?: string; number?: number; u?: number; heightU?: number; label?: string; address?: string; slots?: number; }
  export interface LayoutSubject { kind?: hostkeeper_operator_v1.LayoutSubjectKind; id?: string; }
  export type LayoutSubjectKind = "LAYOUT_SUBJECT_KIND_UNSPECIFIED" | "LAYOUT_SUBJECT_KIND_HOST" | "LAYOUT_SUBJECT_KIND_INVENTORY";
  export type LinuxScope = "LINUX_SCOPE_UNSPECIFIED" | "LINUX_SCOPE_SECURITY" | "LINUX_SCOPE_ALL";
  export interface ListAlertsRequest {}
  export interface ListAlertsResponse { alerts?: hostkeeper_operator_v1.Alert[]; }
  export interface ListApiTokensRequest {}
  export interface ListApiTokensResponse { tokens?: hostkeeper_operator_v1.ApiTokenInfo[]; }
  export interface ListAuditRequest { sinceSeq?: string; limit?: number; actor?: string; action?: string; service?: string; nodeId?: string; byName?: string; newest?: boolean; hostGroup?: string; }
  export interface ListAuditResponse { entries?: hostkeeper_operator_v1.AuditEntry[]; nextSeq?: string; }
  export interface ListConsoleSessionsRequest {}
  export interface ListConsoleSessionsResponse { sessions?: hostkeeper_operator_v1.ConsoleSession[]; }
  export interface ListDevicesRequest { limit?: number; pageToken?: string; }
  export interface ListDevicesResponse { devices?: hostkeeper_operator_v1.MdmDevice[]; nextPageToken?: string; gate?: boolean; configured?: boolean; missing?: hostkeeper_operator_v1.MdmCondition[]; }
  export interface ListEnrollmentRequestsRequest {}
  export interface ListEnrollmentRequestsResponse { requests?: hostkeeper_operator_v1.EnrollmentRequest[]; }
  export interface ListHostGroupsRequest { limit?: number; pageToken?: string; }
  export interface ListHostGroupsResponse { hostGroups?: hostkeeper_operator_v1.HostGroupState[]; nextPageToken?: string; }
  export interface ListHostsRequest { limit?: number; pageToken?: string; }
  export interface ListHostsResponse { hosts?: hostkeeper_operator_v1.Host[]; nextPageToken?: string; }
  export interface ListMaintenanceRunsRequest { states?: hostkeeper_operator_v1.MaintenanceState[]; limit?: number; }
  export interface ListMaintenanceRunsResponse { runs?: hostkeeper_operator_v1.MaintenanceRun[]; }
  export interface ListPendingRequest { limit?: number; pageToken?: string; unenrolled?: boolean; }
  export interface ListPendingResponse { requests?: hostkeeper_operator_v1.PendingEnrollment[]; nextPageToken?: string; }
  export interface ListRecordsRequest { limit?: number; pageToken?: string; enrolled?: boolean; }
  export interface ListRecordsResponse { records?: hostkeeper_operator_v1.InventoryRecord[]; nextPageToken?: string; }
  export interface ListRepositoriesRequest {}
  export interface ListRepositoriesResponse { repositories?: hostkeeper_operator_v1.RegisteredRepository[]; generation?: string; unregistered?: string[]; }
  export interface ListSecretsRequest {}
  export interface ListSecretsResponse { secrets?: hostkeeper_operator_v1.SecretInfo[]; sealing?: boolean; }
  export interface ListServicesRequest {}
  export interface ListServicesResponse { services?: hostkeeper_operator_v1.ServiceEntry[]; holds?: hostkeeper_operator_v1.HoldMark[]; }
  export interface ListSettingsRequest {}
  export interface ListSettingsResponse { settings?: hostkeeper_operator_v1.MdmSetting[]; }
  export interface ListTrustedPublishersRequest { service?: string; }
  export interface ListTrustedPublishersResponse { publishers?: hostkeeper_operator_v1.TrustedPublisher[]; }
  export interface ListUsersRequest {}
  export interface ListUsersResponse { users?: hostkeeper_operator_v1.UserInfo[]; }
  export interface ListXcodeRequest {}
  export interface ListXcodeResponse {
    configured?: boolean;
    archives?: hostkeeper_operator_v1.XcodeArchive[];
    waiting?: hostkeeper_operator_v1.XcodeReleaseWaiting;
    keep?: number;
    maxBytes?: string;
    uploads?: hostkeeper_operator_v1.XcodeUpload[];
  }
  export type LiveReason = "LIVE_REASON_UNSPECIFIED" | "LIVE_REASON_UPDATING" | "LIVE_REASON_PROMOTION_PENDING" | "LIVE_REASON_ORDERED" | "LIVE_REASON_BAKING" | "LIVE_REASON_PLAN_UNDER_WAY" | "LIVE_REASON_QUEUED";
  export interface LiveRollout { service?: string; reason?: hostkeeper_operator_v1.LiveReason; nodeId?: string; }
  export interface MaintenanceHost {
    hostId?: string;
    hostGroup?: string;
    step?: hostkeeper_operator_v1.MaintenanceHostStep;
    sinceUnix?: string;
    kinds?: hostkeeper_agent_v1.MaintenanceKind[];
    applied?: hostkeeper_agent_v1.MaintenanceKind[];
    skippedBecause?: hostkeeper_operator_v1.MaintenanceSkip;
    reason?: string;
  }
  export type MaintenanceHostStep =
    | "MAINTENANCE_HOST_STEP_UNSPECIFIED"
    | "MAINTENANCE_HOST_STEP_WAITING"
    | "MAINTENANCE_HOST_STEP_STAGING"
    | "MAINTENANCE_HOST_STEP_DRAINING"
    | "MAINTENANCE_HOST_STEP_APPLYING"
    | "MAINTENANCE_HOST_STEP_REBOOTING"
    | "MAINTENANCE_HOST_STEP_CHECKING"
    | "MAINTENANCE_HOST_STEP_PUTTING_BACK"
    | "MAINTENANCE_HOST_STEP_DONE"
    | "MAINTENANCE_HOST_STEP_DEFERRED"
    | "MAINTENANCE_HOST_STEP_HALTED"
    | "MAINTENANCE_HOST_STEP_NOT_BACK"
    | "MAINTENANCE_HOST_STEP_NOT_REACHED"
    | "MAINTENANCE_HOST_STEP_SKIPPED";
  export interface MaintenancePolicy {
    xcodeKeep?: number;
    xcodeDefault?: string;
    macosMajor?: boolean;
    linuxScope?: hostkeeper_operator_v1.LinuxScope;
    batch?: number;
    minServing?: number;
    drainDeadlineSecs?: string;
    returnSecs?: string;
  }
  export interface MaintenanceRun {
    id?: string;
    state?: hostkeeper_operator_v1.MaintenanceState;
    kinds?: hostkeeper_agent_v1.MaintenanceKind[];
    hostIds?: string[];
    hostGroup?: string;
    windowStartUnix?: string;
    windowEndUnix?: string;
    batch?: number;
    drainDeadlineSecs?: string;
    takeLastHost?: boolean;
    reason?: string;
    byName?: string;
    scheduledUnix?: string;
    endedUnix?: string;
    haltedGroups?: string[];
    hostCount?: number;
    xcodeSha256?: string;
  }
  export type MaintenanceSkip =
    | "MAINTENANCE_SKIP_UNSPECIFIED"
    | "MAINTENANCE_SKIP_NO_MAINTENANCE_V1"
    | "MAINTENANCE_SKIP_HELD"
    | "MAINTENANCE_SKIP_EXCLUDED"
    | "MAINTENANCE_SKIP_MDM_CONFLICT"
    | "MAINTENANCE_SKIP_NO_UPDATE_ACCOUNT"
    | "MAINTENANCE_SKIP_IN_PACKAGE_UPDATE"
    | "MAINTENANCE_SKIP_GROUP_CHANGED"
    | "MAINTENANCE_SKIP_FILEVAULT_NOT_READY";
  export type MaintenanceState =
    | "MAINTENANCE_STATE_UNSPECIFIED"
    | "MAINTENANCE_STATE_SCHEDULED"
    | "MAINTENANCE_STATE_RUNNING"
    | "MAINTENANCE_STATE_HALTED"
    | "MAINTENANCE_STATE_DONE"
    | "MAINTENANCE_STATE_CANCELLED"
    | "MAINTENANCE_STATE_WINDOW_ENDED";
  export interface MarkBadRequest { service?: string; version?: string; reason?: string; }
  export interface MarkBadResponse { entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface MdmAbmCheck { result?: hostkeeper_operator_v1.MdmAbmResult; organization?: string; detail?: string; }
  export type MdmAbmResult =
    | "MDM_ABM_RESULT_UNSPECIFIED"
    | "MDM_ABM_RESULT_NO_TOKEN"
    | "MDM_ABM_RESULT_NOT_CHECKED"
    | "MDM_ABM_RESULT_ACCEPTED"
    | "MDM_ABM_RESULT_REJECTED"
    | "MDM_ABM_RESULT_UNREACHABLE"
    | "MDM_ABM_RESULT_UNREADABLE";
  export type MdmBaseline = "MDM_BASELINE_UNSPECIFIED" | "MDM_BASELINE_PENDING" | "MDM_BASELINE_APPLIED" | "MDM_BASELINE_FAILED";
  export type MdmBaselineState = "MDM_BASELINE_STATE_UNSPECIFIED" | "MDM_BASELINE_STATE_PENDING" | "MDM_BASELINE_STATE_APPLIED" | "MDM_BASELINE_STATE_FAILED";
  export interface MdmCommand { requestType?: string; commandUuid?: string; sentUnix?: string; }
  export type MdmCondition =
    | "MDM_CONDITION_UNSPECIFIED"
    | "MDM_CONDITION_PUSH_CERT_MISSING"
    | "MDM_CONDITION_PUSH_KEY_MISSING"
    | "MDM_CONDITION_PUSH_PAIR_INVALID"
    | "MDM_CONDITION_PUSH_CERT_EXPIRED"
    | "MDM_CONDITION_ABM_TOKEN_MISSING"
    | "MDM_CONDITION_ABM_TOKEN_UNREADABLE"
    | "MDM_CONDITION_ABM_TOKEN_EXPIRED"
    | "MDM_CONDITION_NAME_MISSING"
    | "MDM_CONDITION_TLS_CERT_MISSING"
    | "MDM_CONDITION_TLS_KEY_MISSING"
    | "MDM_CONDITION_TLS_PAIR_INVALID"
    | "MDM_CONDITION_TLS_CERT_EXPIRED"
    | "MDM_CONDITION_TLS_NAME_MISMATCH"
    | "MDM_CONDITION_PUSH_CERT_NOT_YET_VALID"
    | "MDM_CONDITION_TLS_CERT_NOT_YET_VALID";
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
    baselineState?: hostkeeper_operator_v1.MdmBaselineState;
    baselineReason?: string;
    hostId?: string;
    group?: string;
    baseline?: hostkeeper_operator_v1.MdmBaseline;
  }
  export type MdmPhase = "MDM_PHASE_UNSPECIFIED" | "MDM_PHASE_AUTHENTICATED" | "MDM_PHASE_ENROLLED" | "MDM_PHASE_CHECKED_OUT";
  export interface MdmPushCheck { enrollmentId?: string; result?: hostkeeper_operator_v1.MdmPushResult; detail?: string; apnsId?: string; topic?: string; }
  export type MdmPushResult =
    | "MDM_PUSH_RESULT_UNSPECIFIED"
    | "MDM_PUSH_RESULT_SENT"
    | "MDM_PUSH_RESULT_NO_PUSH_IDENTITY"
    | "MDM_PUSH_RESULT_NO_LISTENER"
    | "MDM_PUSH_RESULT_NO_SUCH_ENROLLMENT"
    | "MDM_PUSH_RESULT_NO_PUSH_TOKEN"
    | "MDM_PUSH_RESULT_BAD_DEVICE_TOKEN"
    | "MDM_PUSH_RESULT_UNREGISTERED"
    | "MDM_PUSH_RESULT_TOPIC_DISALLOWED"
    | "MDM_PUSH_RESULT_BAD_CERTIFICATE_ENVIRONMENT"
    | "MDM_PUSH_RESULT_CERTIFICATE_REJECTED"
    | "MDM_PUSH_RESULT_NO_CLIENT_CERTIFICATE"
    | "MDM_PUSH_RESULT_THROTTLED"
    | "MDM_PUSH_RESULT_APNS_UNAVAILABLE"
    | "MDM_PUSH_RESULT_REJECTED"
    | "MDM_PUSH_RESULT_CONNECTION_FAILED";
  export interface MdmSetting { name?: string; value?: string; isDefault?: boolean; }
  export interface MdmStatusRequest {}
  export interface MdmStatusResponse {
    configured?: boolean;
    missing?: hostkeeper_operator_v1.MdmCondition[];
    name?: string;
    enrollmentUrl?: string;
    nameLocked?: boolean;
    sealing?: boolean;
    pushCert?: hostkeeper_operator_v1.SecretInfo;
    pushKey?: hostkeeper_operator_v1.SecretInfo;
    abmToken?: hostkeeper_operator_v1.SecretInfo;
    tlsCert?: hostkeeper_operator_v1.SecretInfo;
    tlsKey?: hostkeeper_operator_v1.SecretInfo;
    pushTopic?: string;
    pushNotAfterUnix?: string;
    abmNotAfterUnix?: string;
    tlsNotAfterUnix?: string;
    tlsNames?: string[];
    abmLastSyncUnix?: string;
    abmSyncError?: string;
    abmUnavailable?: boolean;
    dnsCredential?: hostkeeper_operator_v1.SecretInfo;
    tlsRenewal?: hostkeeper_operator_v1.TlsRenewal;
  }
  export type Membership = "MEMBERSHIP_UNSPECIFIED" | "MEMBERSHIP_PRESENT" | "MEMBERSHIP_EXCLUDED" | "MEMBERSHIP_NOT_SELECTED" | "MEMBERSHIP_DOWN" | "MEMBERSHIP_ABSENT" | "MEMBERSHIP_MISSING" | "MEMBERSHIP_UNMANAGED";
  export interface MinVersion { role?: string; host?: string; version?: string; }
  export interface MoveHostRequest { nodeId?: string; hostGroup?: string; reason?: string; }
  export interface MoveHostResponse { entry?: hostkeeper_operator_v1.AuditEntry; fromHostGroup?: string; }
  export interface NetworkDevice { id?: string; name?: string; role?: hostkeeper_operator_v1.NetworkRole; consoleHost?: string; consoleDevice?: string; managementMac?: string; notes?: string; }
  export type NetworkRole = "NETWORK_ROLE_UNSPECIFIED" | "NETWORK_ROLE_BMC" | "NETWORK_ROLE_DATA";
  export interface NotYet { hosts?: hostkeeper_operator_v1.WaitingHost[]; hostsLeftOut?: number; plans?: hostkeeper_operator_v1.WaitingPlan[]; answeredUnix?: string; }
  export interface OpenConsoleRequest { hostId?: string; reason?: string; }
  export interface OpenConsoleResponse { kind?: hostkeeper_operator_v1.ConsoleKind; session?: string; expiresInSecs?: string; quietSecs?: string; address?: hostkeeper_operator_v1.ConsoleAddress; }
  export interface OpenRecordRequest { hostId?: string; approveUntilUnix?: string; approveForSecs?: string; reason?: string; }
  export interface OpenRecordResponse { record?: hostkeeper_operator_v1.InventoryRecord; entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface PauseRequest { service?: string; reason?: string; hostGroup?: string; }
  export interface PauseResponse { entry?: hostkeeper_operator_v1.AuditEntry; plan?: hostkeeper_operator_v1.RolloutPlan; }
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
    recordHostGroup?: string;
  }
  export interface PinHostKeysRequest { hostId?: string; fingerprints?: string[]; }
  export interface PlaceLayoutRequest { subject?: hostkeeper_operator_v1.LayoutSubject; rack?: string; heightU?: number; u?: number; reason?: string; sled?: string; slot?: number; }
  export interface PlaceLayoutResponse { placement?: hostkeeper_operator_v1.LayoutPlacement; entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface PlanHost { nodeId?: string; role?: string; state?: hostkeeper_operator_v1.PlanHostState; deferring?: boolean; }
  export type PlanHostState = "PLAN_HOST_STATE_UNSPECIFIED" | "PLAN_HOST_STATE_WAITING" | "PLAN_HOST_STATE_UPDATING" | "PLAN_HOST_STATE_BAKING" | "PLAN_HOST_STATE_DONE" | "PLAN_HOST_STATE_FAILED";
  export interface PlanStage { number?: number; percent?: number; hosts?: number; advance?: hostkeeper_operator_v1.StageAdvance; members?: hostkeeper_operator_v1.PlanHost[]; }
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
    | "PLAN_WAIT_BAKING"
    | "PLAN_WAIT_NOTHING_TO_ORDER";
  export interface PolicyStage { percent?: number; advance?: hostkeeper_operator_v1.StageAdvance; }
  export type PowerAction = "POWER_ACTION_UNSPECIFIED" | "POWER_ACTION_STATUS" | "POWER_ACTION_ON" | "POWER_ACTION_OFF" | "POWER_ACTION_SOFT" | "POWER_ACTION_CYCLE" | "POWER_ACTION_RESET";
  export type PowerState = "POWER_STATE_UNSPECIFIED" | "POWER_STATE_ON" | "POWER_STATE_OFF";
  export interface ProbeConsoleRequest { hostId?: string; reason?: string; waitSecs?: number; quietSecs?: number; maxBytes?: number; }
  export interface ProbeConsoleResponse { findings?: hostkeeper_operator_v1.ConsoleProbeFinding[]; fromBoot?: boolean; bytesRead?: string; ended?: hostkeeper_operator_v1.ConsoleProbeEnd; cause?: string; }
  export interface PutAbmTokenRequest { tokenJson?: string; tokenP7m?: string; }
  export interface PutAbmTokenResponse { secret?: hostkeeper_operator_v1.SecretInfo; serverName?: string; orgName?: string; }
  export interface PutBackMaintenanceHostRequest { runId?: string; hostId?: string; reason?: string; }
  export interface PutBackMaintenanceHostResponse { host?: hostkeeper_operator_v1.MaintenanceHost; entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface PutHostGroupRequest { hostGroup?: hostkeeper_operator_v1.HostGroup; ifGeneration?: string; reason?: string; }
  export interface PutHostGroupResponse { entry?: hostkeeper_operator_v1.AuditEntry; generation?: string; }
  export interface PutLayoutDeviceRequest { id?: string; name?: string; role?: hostkeeper_operator_v1.NetworkRole; consoleHost?: string; consoleDevice?: string; managementMac?: string; notes?: string; reason?: string; }
  export interface PutLayoutDeviceResponse { device?: hostkeeper_operator_v1.NetworkDevice; entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface PutLayoutNameRequest { name?: string; reason?: string; }
  export interface PutLayoutNameResponse { name?: string; entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface PutLayoutRackRequest { id?: string; row?: string; heightU?: number; reason?: string; }
  export interface PutLayoutRackResponse { rack?: hostkeeper_operator_v1.LayoutRack; entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface PutLayoutSledRequest { id?: string; rack?: string; u?: number; heightU?: number; label?: string; reason?: string; slots?: number; }
  export interface PutLayoutSledResponse { sled?: hostkeeper_operator_v1.LayoutSled; entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface PutNameRequest { name?: string; }
  export interface PutNameResponse { name?: string; enrollmentUrl?: string; }
  export interface PutPushCertificateRequest { certificatePem?: string; keyPem?: string; }
  export interface PutPushCertificateResponse { topic?: string; notBeforeUnix?: string; notAfterUnix?: string; secret?: hostkeeper_operator_v1.SecretInfo; renewal?: boolean; }
  export interface PutPushReplyRequest { reply?: string; }
  export interface PutPushReplyResponse { signedRequest?: string; }
  export interface PutRepositoryRequest { repository?: hostkeeper_release_v1.Repository; reason?: string; }
  export interface PutRepositoryResponse { repository?: hostkeeper_operator_v1.RegisteredRepository; entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface PutSecretRequest { kind?: string; material?: string; notAfterUnix?: string; withCertificate?: string; }
  export interface PutSecretResponse { secret?: hostkeeper_operator_v1.SecretInfo; }
  export interface PutServiceRequest { service?: string; repository?: string; reason?: string; }
  export interface PutServiceResponse { entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface PutSettingRequest { name?: string; value?: string; }
  export interface PutSettingResponse { name?: string; value?: string; }
  export interface PutTrustedPublisherRequest { service?: string; repository?: string; workflow?: string; ref?: string; environment?: string; reason?: string; }
  export interface PutTrustedPublisherResponse { entry?: hostkeeper_operator_v1.AuditEntry; publishers?: hostkeeper_operator_v1.TrustedPublisher[]; }
  export type RecordState = "RECORD_STATE_UNSPECIFIED" | "RECORD_STATE_OPEN" | "RECORD_STATE_CLOSED" | "RECORD_STATE_USED";
  export interface RegisteredRepository { repository?: hostkeeper_release_v1.Repository; keyringSha256?: string; generation?: string; services?: string[]; builtin?: boolean; platforms?: string[]; }
  export interface RejectRequest { requestId?: string; reason?: string; }
  export interface RejectResponse { entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface RemoveHostGroupRequest { hostGroup?: string; reason?: string; }
  export interface RemoveHostGroupResponse { entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface RemoveLayoutDeviceRequest { id?: string; reason?: string; }
  export interface RemoveLayoutDeviceResponse { entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface RemoveLayoutRackRequest { id?: string; reason?: string; }
  export interface RemoveLayoutRackResponse { entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface RemoveLayoutRowRequest { id?: string; reason?: string; }
  export interface RemoveLayoutRowResponse { entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface RemoveLayoutSledRequest { id?: string; reason?: string; }
  export interface RemoveLayoutSledResponse { entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface RemoveRecordRequest { hostId?: string; reason?: string; }
  export interface RemoveRecordResponse { entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface RemoveRepositoryRequest { name?: string; reason?: string; }
  export interface RemoveRepositoryResponse { generation?: string; entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface RemoveServiceRequest { service?: string; reason?: string; }
  export interface RemoveServiceResponse { entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface RemoveTrustedPublisherRequest { service?: string; repository?: string; workflow?: string; ref?: string; environment?: string; reason?: string; }
  export interface RemoveTrustedPublisherResponse { entry?: hostkeeper_operator_v1.AuditEntry; publishers?: hostkeeper_operator_v1.TrustedPublisher[]; }
  export interface RemoveXcodeRequest { sha256?: string; reason?: string; }
  export interface RemoveXcodeResponse { entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface Replaced { version?: string; minUp?: { [key: string]: number }; }
  export interface ResetPasswordRequest { name?: string; }
  export interface ResetPasswordResponse { user?: hostkeeper_operator_v1.UserInfo; password?: string; }
  export interface ResumeRequest { service?: string; reason?: string; hostGroup?: string; }
  export interface ResumeResponse { entry?: hostkeeper_operator_v1.AuditEntry; plan?: hostkeeper_operator_v1.RolloutPlan; }
  export interface RetireRequest { nodeId?: string; reason?: string; }
  export interface RetireResponse { entry?: hostkeeper_operator_v1.AuditEntry; updateInFlight?: boolean; }
  export interface RetryMaintenanceHostRequest { runId?: string; hostId?: string; reason?: string; }
  export interface RetryMaintenanceHostResponse { host?: hostkeeper_operator_v1.MaintenanceHost; entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface RetryRequest { nodeId?: string; service?: string; }
  export interface RetryResponse { setAside?: hostkeeper_agent_v1.UpdateResult; entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface RevokeApiTokenRequest { name?: string; }
  export interface RevokeApiTokenResponse {}
  export interface RevokeRequest { operator?: string; serial?: string; }
  export interface RevokeResponse { revoked?: string; }
  export interface RoleCounts { role?: string; atDesired?: number; updating?: number; failed?: number; down?: number; pinned?: number; }
  export type RollbackMode = "ROLLBACK_MODE_UNSPECIFIED" | "ROLLBACK_MODE_MANUAL" | "ROLLBACK_MODE_AUTOMATIC";
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
    waitingOn?: string[];
    hostGroup?: string;
    waitingOnWhy?: hostkeeper_operator_v1.LiveRollout[];
    queuePosition?: number;
  }
  export interface RolloutPolicy {
    stages?: hostkeeper_operator_v1.PolicyStage[];
    canary?: hostkeeper_operator_v1.Canary;
    bakeBeats?: number;
    rollback?: hostkeeper_operator_v1.RollbackMode;
    rollbackAfterFailed?: hostkeeper_operator_v1.FailThreshold;
    deferBackoff?: hostkeeper_operator_v1.DeferBackoff;
    maxUpdating?: number;
    minUp?: number;
    liveRollouts?: number;
    maintenance?: hostkeeper_operator_v1.MaintenancePolicy;
    failedHosts?: hostkeeper_operator_v1.FailedHosts;
  }
  export interface RunningService { service?: string; version?: string; installer?: string; }
  export interface ScheduleMaintenanceRequest {
    kinds?: hostkeeper_agent_v1.MaintenanceKind[];
    hostIds?: string[];
    hostGroup?: string;
    windowStartUnix?: string;
    windowEndUnix?: string;
    batch?: number;
    drainDeadlineSecs?: string;
    reason?: string;
    takeLastHost?: boolean;
    confirm?: boolean;
    xcodeSha256?: string;
  }
  export interface ScheduleMaintenanceResponse { run?: hostkeeper_operator_v1.MaintenanceRun; hosts?: hostkeeper_operator_v1.MaintenanceHost[]; entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface SecretInfo { kind?: string; version?: number; fingerprintSha256?: string; notAfterUnix?: string; createdUnix?: string; supersededUnix?: string; }
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
    repository?: string;
    hostGroups?: string[];
  }
  export interface ServiceHost { nodeId?: string; role?: string; live?: boolean; view?: hostkeeper_operator_v1.HostService; beat?: hostkeeper_agent_v1.ServiceBeat; excludedReason?: string; }
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
    hostGroup?: string;
  }
  export interface SetBootDeviceRequest { hostId?: string; device?: hostkeeper_operator_v1.BootDevice; persistent?: boolean; efi?: boolean; reason?: string; }
  export interface SetBootDeviceResponse { device?: hostkeeper_operator_v1.BootDevice; persistent?: boolean; efi?: boolean; address?: hostkeeper_operator_v1.ConsoleAddress; }
  export interface SetDesiredRequest {
    service?: string;
    desired?: hostkeeper_operator_v1.DesiredState;
    ifGeneration?: string;
    writeId?: string;
    packageDigest?: string;
    releaseIndex?: string;
    releaseIndexDigest?: string;
    targets?: { [key: string]: hostkeeper_release_v1.TargetList };
    hostGroup?: string;
  }
  export interface SetDesiredResponse { desired?: hostkeeper_operator_v1.DesiredState; record?: hostkeeper_operator_v1.WriteRecord; droppedRoles?: string[]; }
  export interface SetLabelsRequest { nodeId?: string; labels?: { [key: string]: string }; }
  export interface SetLabelsResponse { entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface SetMinVersionRequest { service?: string; minVersion?: hostkeeper_operator_v1.MinVersion; }
  export interface SetMinVersionResponse { entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface SetRepositoryRequest { service?: string; repository?: string; reason?: string; }
  export interface SetRepositoryResponse { entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface SignUserCertRequest { publicKey?: string; reason?: string; hostId?: string; }
  export interface SignUserCertResponse { certificate?: string; loginUser?: string; expiresUnix?: string; keyId?: string; }
  export interface SkippedHost { nodeId?: string; reason?: string; }
  export type StageAdvance = "STAGE_ADVANCE_UNSPECIFIED" | "STAGE_ADVANCE_AUTO" | "STAGE_ADVANCE_MANUAL";
  export interface StatusRequest { service?: string; }
  export interface StatusResponse { services?: hostkeeper_operator_v1.ServiceStatus[]; quiet?: boolean; servicesDropped?: string; factsDropped?: { [key: string]: string }; audit?: hostkeeper_operator_v1.AuditEntry[]; }
  export interface TlsRenewal { sinceUnix?: string; attempts?: number; message?: string; dnsRejected?: string; retryAtUnix?: string; }
  export interface TrustedPublisher { repository?: string; workflow?: string; ref?: string; environment?: string; repositoryId?: string; calledWorkflows?: string[]; limitedToHostGroups?: boolean; hostGroups?: string[]; }
  export interface UnassignRequest { nodeId?: string; service?: string; uninstall?: boolean; role?: string; }
  export interface UnassignResponse { entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface UnholdRequest { scope?: hostkeeper_operator_v1.HoldScope; service?: string; reason?: string; }
  export interface UnholdResponse { entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface UnmarkBadRequest { service?: string; version?: string; reason?: string; }
  export interface UnmarkBadResponse { entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface UnplaceLayoutRequest { subject?: hostkeeper_operator_v1.LayoutSubject; reason?: string; }
  export interface UnplaceLayoutResponse { entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface UpdateRecordRequest { hostId?: string; dataMacs?: string[]; bmcMac?: string; notes?: string; }
  export interface UpdateRecordResponse { record?: hostkeeper_operator_v1.InventoryRecord; entry?: hostkeeper_operator_v1.AuditEntry; }
  export interface UserInfo { name?: string; enabled?: boolean; mustChange?: boolean; createdBy?: string; createdUnix?: string; lastLoginUnix?: string; passwordValidUntilUnix?: string; }
  export interface VerifyRequest { enrollmentId?: string; }
  export interface VerifyResponse {
    status?: hostkeeper_operator_v1.MdmStatusResponse;
    listener?: boolean;
    serving?: boolean;
    servedNames?: string[];
    servedNamesTheName?: boolean;
    abm?: hostkeeper_operator_v1.MdmAbmCheck;
    push?: hostkeeper_operator_v1.MdmPushCheck;
  }
  export interface WaitConvergedRequest { service?: string; version?: string; waitSecs?: number; hostGroup?: string; }
  export interface WaitConvergedResponse { outcome?: hostkeeper_operator_v1.WaitOutcome; convergence?: hostkeeper_operator_v1.Convergence; notYet?: hostkeeper_operator_v1.NotYet; }
  export type WaitOutcome = "WAIT_OUTCOME_UNSPECIFIED" | "WAIT_OUTCOME_CONVERGED" | "WAIT_OUTCOME_HALTED" | "WAIT_OUTCOME_SUPERSEDED" | "WAIT_OUTCOME_DEADLINE" | "WAIT_OUTCOME_NO_DESIRED";
  export interface WaitingHost {
    nodeId?: string;
    role?: string;
    state?: hostkeeper_operator_v1.WaitingState;
    installedVersion?: string;
    updatingTo?: string;
    step?: string;
    stepAttempt?: number;
    reason?: string;
    sinceUnix?: string;
  }
  export interface WaitingPlan { hostGroup?: string; waiting?: hostkeeper_operator_v1.PlanWait; waitingOn?: string[]; pauseReason?: string; }
  export type WaitingState = "WAITING_STATE_UNSPECIFIED" | "WAITING_STATE_UPDATING" | "WAITING_STATE_FAILED" | "WAITING_STATE_EXCLUDED" | "WAITING_STATE_SILENT" | "WAITING_STATE_BEHIND" | "WAITING_STATE_SETTLING";
  export interface WholeFleet {}
  export interface WriteRecord { generation?: string; replaced?: hostkeeper_operator_v1.Replaced; writeId?: string; }
  export interface XcodeArchive { sha256?: string; version?: string; build?: string; sizeBytes?: string; uploadedBy?: string; uploadedAtUnix?: string; }
  export interface XcodeReleaseWaiting { version?: string; build?: string; requiresMacos?: string; }
  export interface XcodeUpload { sha256?: string; version?: string; build?: string; receivedBytes?: string; sizeBytes?: string; startedBy?: string; lastWriteUnix?: string; }
}
