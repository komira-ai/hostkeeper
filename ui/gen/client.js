// Generated from hostkeeper-proto's descriptors by hostkeeper-ui-tests (`client_gen`). Do not
// edit: `cargo test -p hostkeeper-ui-tests --test client` fails when this file differs from what
// the descriptors make, and writes the current text beside its test binary.
//
// `createClient(call)` returns one object per service of the operator API, each with one
// function per method: `releases.setDesired(request)` is
// `call("hostkeeper.operator.v1.Releases/SetDesired", request)`, and returns what `call`
// returns. `ui/api.js` gives it the `call` that sends the request. The types of every
// request and reply are in `client.d.ts`.

export function createClient(call) {
  return Object.freeze({
    alerts: Object.freeze({
      list: (request) => call("hostkeeper.operator.v1.Alerts/List", request),
    }),
    apiTokens: Object.freeze({
      list: (request) => call("hostkeeper.operator.v1.ApiTokens/List", request),
      create: (request) => call("hostkeeper.operator.v1.ApiTokens/Create", request),
      revoke: (request) => call("hostkeeper.operator.v1.ApiTokens/Revoke", request),
    }),
    audit: Object.freeze({
      list: (request) => call("hostkeeper.operator.v1.Audit/List", request),
    }),
    enrollment: Object.freeze({
      list: (request) => call("hostkeeper.operator.v1.Enrollment/List", request),
      approve: (request) => call("hostkeeper.operator.v1.Enrollment/Approve", request),
      reject: (request) => call("hostkeeper.operator.v1.Enrollment/Reject", request),
      addRecord: (request) => call("hostkeeper.operator.v1.Enrollment/AddRecord", request),
      listRecords: (request) => call("hostkeeper.operator.v1.Enrollment/ListRecords", request),
      removeRecord: (request) => call("hostkeeper.operator.v1.Enrollment/RemoveRecord", request),
      openRecord: (request) => call("hostkeeper.operator.v1.Enrollment/OpenRecord", request),
    }),
    hostGroups: Object.freeze({
      list: (request) => call("hostkeeper.operator.v1.HostGroups/List", request),
      get: (request) => call("hostkeeper.operator.v1.HostGroups/Get", request),
      put: (request) => call("hostkeeper.operator.v1.HostGroups/Put", request),
      remove: (request) => call("hostkeeper.operator.v1.HostGroups/Remove", request),
      move: (request) => call("hostkeeper.operator.v1.HostGroups/Move", request),
    }),
    hosts: Object.freeze({
      list: (request) => call("hostkeeper.operator.v1.Hosts/List", request),
      get: (request) => call("hostkeeper.operator.v1.Hosts/Get", request),
      exclude: (request) => call("hostkeeper.operator.v1.Hosts/Exclude", request),
      include: (request) => call("hostkeeper.operator.v1.Hosts/Include", request),
      setLabels: (request) => call("hostkeeper.operator.v1.Hosts/SetLabels", request),
      abort: (request) => call("hostkeeper.operator.v1.Hosts/Abort", request),
      retire: (request) => call("hostkeeper.operator.v1.Hosts/Retire", request),
      clearRecord: (request) => call("hostkeeper.operator.v1.Hosts/ClearRecord", request),
      retry: (request) => call("hostkeeper.operator.v1.Hosts/Retry", request),
    }),
    mdm: Object.freeze({
      listDevices: (request) => call("hostkeeper.operator.v1.Mdm/ListDevices", request),
      putSecret: (request) => call("hostkeeper.operator.v1.Mdm/PutSecret", request),
      putSetting: (request) => call("hostkeeper.operator.v1.Mdm/PutSetting", request),
      listSecrets: (request) => call("hostkeeper.operator.v1.Mdm/ListSecrets", request),
      listSettings: (request) => call("hostkeeper.operator.v1.Mdm/ListSettings", request),
      createPushRequest: (request) => call("hostkeeper.operator.v1.Mdm/CreatePushRequest", request),
      putPushReply: (request) => call("hostkeeper.operator.v1.Mdm/PutPushReply", request),
      putPushCertificate: (request) => call("hostkeeper.operator.v1.Mdm/PutPushCertificate", request),
      status: (request) => call("hostkeeper.operator.v1.Mdm/Status", request),
      putName: (request) => call("hostkeeper.operator.v1.Mdm/PutName", request),
      verify: (request) => call("hostkeeper.operator.v1.Mdm/Verify", request),
      putAbmToken: (request) => call("hostkeeper.operator.v1.Mdm/PutAbmToken", request),
      getAbmPublicKey: (request) => call("hostkeeper.operator.v1.Mdm/GetAbmPublicKey", request),
      listEnrollmentRequests: (request) => call("hostkeeper.operator.v1.Mdm/ListEnrollmentRequests", request),
      confirmEnrollment: (request) => call("hostkeeper.operator.v1.Mdm/ConfirmEnrollment", request),
    }),
    releases: Object.freeze({
      setDesired: (request) => call("hostkeeper.operator.v1.Releases/SetDesired", request),
      converge: (request) => call("hostkeeper.operator.v1.Releases/Converge", request),
      force: (request) => call("hostkeeper.operator.v1.Releases/Force", request),
      unmarkBad: (request) => call("hostkeeper.operator.v1.Releases/UnmarkBad", request),
      clearHalt: (request) => call("hostkeeper.operator.v1.Releases/ClearHalt", request),
      status: (request) => call("hostkeeper.operator.v1.Releases/Status", request),
      markBad: (request) => call("hostkeeper.operator.v1.Releases/MarkBad", request),
      halt: (request) => call("hostkeeper.operator.v1.Releases/Halt", request),
      waitConverged: (request) => call("hostkeeper.operator.v1.Releases/WaitConverged", request),
      getRelease: (request) => call("hostkeeper.operator.v1.Releases/GetRelease", request),
    }),
    repositories: Object.freeze({
      list: (request) => call("hostkeeper.operator.v1.Repositories/List", request),
      get: (request) => call("hostkeeper.operator.v1.Repositories/Get", request),
      put: (request) => call("hostkeeper.operator.v1.Repositories/Put", request),
      remove: (request) => call("hostkeeper.operator.v1.Repositories/Remove", request),
    }),
    rollouts: Object.freeze({
      advance: (request) => call("hostkeeper.operator.v1.Rollouts/Advance", request),
      pause: (request) => call("hostkeeper.operator.v1.Rollouts/Pause", request),
      resume: (request) => call("hostkeeper.operator.v1.Rollouts/Resume", request),
    }),
    services: Object.freeze({
      list: (request) => call("hostkeeper.operator.v1.Services/List", request),
      assign: (request) => call("hostkeeper.operator.v1.Services/Assign", request),
      unassign: (request) => call("hostkeeper.operator.v1.Services/Unassign", request),
      setMinVersion: (request) => call("hostkeeper.operator.v1.Services/SetMinVersion", request),
      setRepository: (request) => call("hostkeeper.operator.v1.Services/SetRepository", request),
      put: (request) => call("hostkeeper.operator.v1.Services/Put", request),
      remove: (request) => call("hostkeeper.operator.v1.Services/Remove", request),
      hold: (request) => call("hostkeeper.operator.v1.Services/Hold", request),
      unhold: (request) => call("hostkeeper.operator.v1.Services/Unhold", request),
    }),
    users: Object.freeze({
      list: (request) => call("hostkeeper.operator.v1.Users/List", request),
      create: (request) => call("hostkeeper.operator.v1.Users/Create", request),
      disable: (request) => call("hostkeeper.operator.v1.Users/Disable", request),
      enable: (request) => call("hostkeeper.operator.v1.Users/Enable", request),
      resetPassword: (request) => call("hostkeeper.operator.v1.Users/ResetPassword", request),
      delete: (request) => call("hostkeeper.operator.v1.Users/Delete", request),
    }),
  });
}
