// Generated from hostkeeper-proto's descriptors by hostkeeper-ui-tests (`client_gen`). Do not
// edit: `cargo test -p hostkeeper-ui-tests --test client` fails when this file differs from what
// the descriptors make, and writes the current text beside its test binary.
import type { hostkeeper_operator_v1 } from "./hostkeeper_operator_v1.js";

// The types of `client.js`, in proto3 JSON; each is described in `crates/hostkeeper-proto/proto`.

/** Sends `request` to `method` (`<package>.<Service>/<Method>`) and resolves to the reply. */
export type Call = (method: string, request: object) => Promise<object>;

/** The operator API: one object per service, one function per method. */
export interface Client {
  readonly alerts: {
    list(request: hostkeeper_operator_v1.ListAlertsRequest): Promise<hostkeeper_operator_v1.ListAlertsResponse>;
  };
  readonly apiTokens: {
    list(request: hostkeeper_operator_v1.ListApiTokensRequest): Promise<hostkeeper_operator_v1.ListApiTokensResponse>;
    create(request: hostkeeper_operator_v1.CreateApiTokenRequest): Promise<hostkeeper_operator_v1.CreateApiTokenResponse>;
    revoke(request: hostkeeper_operator_v1.RevokeApiTokenRequest): Promise<hostkeeper_operator_v1.RevokeApiTokenResponse>;
  };
  readonly audit: {
    list(request: hostkeeper_operator_v1.ListAuditRequest): Promise<hostkeeper_operator_v1.ListAuditResponse>;
  };
  readonly console: {
    open(request: hostkeeper_operator_v1.OpenConsoleRequest): Promise<hostkeeper_operator_v1.OpenConsoleResponse>;
    power(request: hostkeeper_operator_v1.ConsolePowerRequest): Promise<hostkeeper_operator_v1.ConsolePowerResponse>;
    setBootDevice(request: hostkeeper_operator_v1.SetBootDeviceRequest): Promise<hostkeeper_operator_v1.SetBootDeviceResponse>;
    list(request: hostkeeper_operator_v1.ListConsoleSessionsRequest): Promise<hostkeeper_operator_v1.ListConsoleSessionsResponse>;
    close(request: hostkeeper_operator_v1.CloseConsoleRequest): Promise<hostkeeper_operator_v1.CloseConsoleResponse>;
  };
  readonly enrollment: {
    list(request: hostkeeper_operator_v1.ListPendingRequest): Promise<hostkeeper_operator_v1.ListPendingResponse>;
    approve(request: hostkeeper_operator_v1.ApproveRequest): Promise<hostkeeper_operator_v1.ApproveResponse>;
    reject(request: hostkeeper_operator_v1.RejectRequest): Promise<hostkeeper_operator_v1.RejectResponse>;
    addRecord(request: hostkeeper_operator_v1.AddRecordRequest): Promise<hostkeeper_operator_v1.AddRecordResponse>;
    listRecords(request: hostkeeper_operator_v1.ListRecordsRequest): Promise<hostkeeper_operator_v1.ListRecordsResponse>;
    removeRecord(request: hostkeeper_operator_v1.RemoveRecordRequest): Promise<hostkeeper_operator_v1.RemoveRecordResponse>;
    updateRecord(request: hostkeeper_operator_v1.UpdateRecordRequest): Promise<hostkeeper_operator_v1.UpdateRecordResponse>;
    openRecord(request: hostkeeper_operator_v1.OpenRecordRequest): Promise<hostkeeper_operator_v1.OpenRecordResponse>;
  };
  readonly hostGroups: {
    list(request: hostkeeper_operator_v1.ListHostGroupsRequest): Promise<hostkeeper_operator_v1.ListHostGroupsResponse>;
    get(request: hostkeeper_operator_v1.GetHostGroupRequest): Promise<hostkeeper_operator_v1.GetHostGroupResponse>;
    put(request: hostkeeper_operator_v1.PutHostGroupRequest): Promise<hostkeeper_operator_v1.PutHostGroupResponse>;
    remove(request: hostkeeper_operator_v1.RemoveHostGroupRequest): Promise<hostkeeper_operator_v1.RemoveHostGroupResponse>;
    move(request: hostkeeper_operator_v1.MoveHostRequest): Promise<hostkeeper_operator_v1.MoveHostResponse>;
  };
  readonly hosts: {
    list(request: hostkeeper_operator_v1.ListHostsRequest): Promise<hostkeeper_operator_v1.ListHostsResponse>;
    get(request: hostkeeper_operator_v1.GetHostRequest): Promise<hostkeeper_operator_v1.GetHostResponse>;
    exclude(request: hostkeeper_operator_v1.ExcludeRequest): Promise<hostkeeper_operator_v1.ExcludeResponse>;
    include(request: hostkeeper_operator_v1.IncludeRequest): Promise<hostkeeper_operator_v1.IncludeResponse>;
    setLabels(request: hostkeeper_operator_v1.SetLabelsRequest): Promise<hostkeeper_operator_v1.SetLabelsResponse>;
    abort(request: hostkeeper_operator_v1.AbortRequest): Promise<hostkeeper_operator_v1.AbortResponse>;
    retire(request: hostkeeper_operator_v1.RetireRequest): Promise<hostkeeper_operator_v1.RetireResponse>;
    clearRecord(request: hostkeeper_operator_v1.ClearRecordRequest): Promise<hostkeeper_operator_v1.ClearRecordResponse>;
    retry(request: hostkeeper_operator_v1.RetryRequest): Promise<hostkeeper_operator_v1.RetryResponse>;
  };
  readonly layout: {
    get(request: hostkeeper_operator_v1.GetLayoutRequest): Promise<hostkeeper_operator_v1.GetLayoutResponse>;
    putName(request: hostkeeper_operator_v1.PutLayoutNameRequest): Promise<hostkeeper_operator_v1.PutLayoutNameResponse>;
    addRow(request: hostkeeper_operator_v1.AddLayoutRowRequest): Promise<hostkeeper_operator_v1.AddLayoutRowResponse>;
    removeRow(request: hostkeeper_operator_v1.RemoveLayoutRowRequest): Promise<hostkeeper_operator_v1.RemoveLayoutRowResponse>;
    putRack(request: hostkeeper_operator_v1.PutLayoutRackRequest): Promise<hostkeeper_operator_v1.PutLayoutRackResponse>;
    removeRack(request: hostkeeper_operator_v1.RemoveLayoutRackRequest): Promise<hostkeeper_operator_v1.RemoveLayoutRackResponse>;
    place(request: hostkeeper_operator_v1.PlaceLayoutRequest): Promise<hostkeeper_operator_v1.PlaceLayoutResponse>;
    unplace(request: hostkeeper_operator_v1.UnplaceLayoutRequest): Promise<hostkeeper_operator_v1.UnplaceLayoutResponse>;
    putDevice(request: hostkeeper_operator_v1.PutLayoutDeviceRequest): Promise<hostkeeper_operator_v1.PutLayoutDeviceResponse>;
    removeDevice(request: hostkeeper_operator_v1.RemoveLayoutDeviceRequest): Promise<hostkeeper_operator_v1.RemoveLayoutDeviceResponse>;
    putSled(request: hostkeeper_operator_v1.PutLayoutSledRequest): Promise<hostkeeper_operator_v1.PutLayoutSledResponse>;
    removeSled(request: hostkeeper_operator_v1.RemoveLayoutSledRequest): Promise<hostkeeper_operator_v1.RemoveLayoutSledResponse>;
  };
  readonly mdm: {
    listDevices(request: hostkeeper_operator_v1.ListDevicesRequest): Promise<hostkeeper_operator_v1.ListDevicesResponse>;
    putSecret(request: hostkeeper_operator_v1.PutSecretRequest): Promise<hostkeeper_operator_v1.PutSecretResponse>;
    putSetting(request: hostkeeper_operator_v1.PutSettingRequest): Promise<hostkeeper_operator_v1.PutSettingResponse>;
    listSecrets(request: hostkeeper_operator_v1.ListSecretsRequest): Promise<hostkeeper_operator_v1.ListSecretsResponse>;
    listSettings(request: hostkeeper_operator_v1.ListSettingsRequest): Promise<hostkeeper_operator_v1.ListSettingsResponse>;
    createPushRequest(request: hostkeeper_operator_v1.CreatePushRequestRequest): Promise<hostkeeper_operator_v1.CreatePushRequestResponse>;
    putPushReply(request: hostkeeper_operator_v1.PutPushReplyRequest): Promise<hostkeeper_operator_v1.PutPushReplyResponse>;
    putPushCertificate(request: hostkeeper_operator_v1.PutPushCertificateRequest): Promise<hostkeeper_operator_v1.PutPushCertificateResponse>;
    status(request: hostkeeper_operator_v1.MdmStatusRequest): Promise<hostkeeper_operator_v1.MdmStatusResponse>;
    putName(request: hostkeeper_operator_v1.PutNameRequest): Promise<hostkeeper_operator_v1.PutNameResponse>;
    verify(request: hostkeeper_operator_v1.VerifyRequest): Promise<hostkeeper_operator_v1.VerifyResponse>;
    putAbmToken(request: hostkeeper_operator_v1.PutAbmTokenRequest): Promise<hostkeeper_operator_v1.PutAbmTokenResponse>;
    getAbmPublicKey(request: hostkeeper_operator_v1.GetAbmPublicKeyRequest): Promise<hostkeeper_operator_v1.GetAbmPublicKeyResponse>;
    listEnrollmentRequests(request: hostkeeper_operator_v1.ListEnrollmentRequestsRequest): Promise<hostkeeper_operator_v1.ListEnrollmentRequestsResponse>;
    confirmEnrollment(request: hostkeeper_operator_v1.ConfirmEnrollmentRequest): Promise<hostkeeper_operator_v1.ConfirmEnrollmentResponse>;
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
    waitConverged(request: hostkeeper_operator_v1.WaitConvergedRequest): Promise<hostkeeper_operator_v1.WaitConvergedResponse>;
    getRelease(request: hostkeeper_operator_v1.GetReleaseRequest): Promise<hostkeeper_operator_v1.GetReleaseResponse>;
  };
  readonly repositories: {
    list(request: hostkeeper_operator_v1.ListRepositoriesRequest): Promise<hostkeeper_operator_v1.ListRepositoriesResponse>;
    get(request: hostkeeper_operator_v1.GetRepositoryRequest): Promise<hostkeeper_operator_v1.GetRepositoryResponse>;
    put(request: hostkeeper_operator_v1.PutRepositoryRequest): Promise<hostkeeper_operator_v1.PutRepositoryResponse>;
    remove(request: hostkeeper_operator_v1.RemoveRepositoryRequest): Promise<hostkeeper_operator_v1.RemoveRepositoryResponse>;
    discover(request: hostkeeper_operator_v1.DiscoverRepositoryRequest): Promise<hostkeeper_operator_v1.DiscoverRepositoryResponse>;
    add(request: hostkeeper_operator_v1.AddRepositoryRequest): Promise<hostkeeper_operator_v1.AddRepositoryResponse>;
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
    setRepository(request: hostkeeper_operator_v1.SetRepositoryRequest): Promise<hostkeeper_operator_v1.SetRepositoryResponse>;
    put(request: hostkeeper_operator_v1.PutServiceRequest): Promise<hostkeeper_operator_v1.PutServiceResponse>;
    remove(request: hostkeeper_operator_v1.RemoveServiceRequest): Promise<hostkeeper_operator_v1.RemoveServiceResponse>;
    listTrustedPublishers(request: hostkeeper_operator_v1.ListTrustedPublishersRequest): Promise<hostkeeper_operator_v1.ListTrustedPublishersResponse>;
    putTrustedPublisher(request: hostkeeper_operator_v1.PutTrustedPublisherRequest): Promise<hostkeeper_operator_v1.PutTrustedPublisherResponse>;
    removeTrustedPublisher(request: hostkeeper_operator_v1.RemoveTrustedPublisherRequest): Promise<hostkeeper_operator_v1.RemoveTrustedPublisherResponse>;
    hold(request: hostkeeper_operator_v1.HoldRequest): Promise<hostkeeper_operator_v1.HoldResponse>;
    unhold(request: hostkeeper_operator_v1.UnholdRequest): Promise<hostkeeper_operator_v1.UnholdResponse>;
  };
  readonly ssh: {
    signUserCert(request: hostkeeper_operator_v1.SignUserCertRequest): Promise<hostkeeper_operator_v1.SignUserCertResponse>;
    hostKeys(request: hostkeeper_operator_v1.HostKeysRequest): Promise<hostkeeper_operator_v1.HostKeysResponse>;
    pinHostKeys(request: hostkeeper_operator_v1.PinHostKeysRequest): Promise<hostkeeper_operator_v1.HostKeysResponse>;
    revoke(request: hostkeeper_operator_v1.RevokeRequest): Promise<hostkeeper_operator_v1.RevokeResponse>;
  };
  readonly users: {
    list(request: hostkeeper_operator_v1.ListUsersRequest): Promise<hostkeeper_operator_v1.ListUsersResponse>;
    create(request: hostkeeper_operator_v1.CreateUserRequest): Promise<hostkeeper_operator_v1.CreateUserResponse>;
    disable(request: hostkeeper_operator_v1.DisableUserRequest): Promise<hostkeeper_operator_v1.DisableUserResponse>;
    enable(request: hostkeeper_operator_v1.EnableUserRequest): Promise<hostkeeper_operator_v1.EnableUserResponse>;
    resetPassword(request: hostkeeper_operator_v1.ResetPasswordRequest): Promise<hostkeeper_operator_v1.ResetPasswordResponse>;
    delete(request: hostkeeper_operator_v1.DeleteUserRequest): Promise<hostkeeper_operator_v1.DeleteUserResponse>;
  };
  readonly xcode: {
    list(request: hostkeeper_operator_v1.ListXcodeRequest): Promise<hostkeeper_operator_v1.ListXcodeResponse>;
    remove(request: hostkeeper_operator_v1.RemoveXcodeRequest): Promise<hostkeeper_operator_v1.RemoveXcodeResponse>;
  };
}

export declare function createClient(call: Call): Client;
