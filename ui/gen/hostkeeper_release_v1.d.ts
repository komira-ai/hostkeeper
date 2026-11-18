// Generated from hostkeeper-proto's descriptors by hostkeeper-ui-tests (`client_gen`). Do not
// edit: `cargo test -p hostkeeper-ui-tests --test client` fails when this file differs from what
// the descriptors make, and writes the current text beside its test binary.
export namespace hostkeeper_release_v1 {
  export interface Blob { digest?: string; bytes?: string; locations?: hostkeeper_release_v1.Location[]; }
  export interface Container { unit?: string; args?: string[]; ports?: hostkeeper_release_v1.Port[]; volumes?: hostkeeper_release_v1.Volume[]; }
  export interface Hook { command?: string[]; boot?: string; timeoutSecs?: hostkeeper_release_v1.StepBounds; verifyDeadlineSecs?: string; promotionCeilingSecs?: string; cleanupRetries?: number; }
  export interface InstallerBounds { stageSecs?: string; activateSecs?: string; removeSecs?: string; }
  export interface Location { store?: string; locator?: string; }
  export interface Manifest {
    package?: string;
    hook?: hostkeeper_release_v1.Hook;
    run?: string[];
    healthUrl?: string;
    place?: hostkeeper_release_v1.Place[];
    params?: { [key: string]: hostkeeper_release_v1.Param };
    minHostkeeper?: string;
    containers?: hostkeeper_release_v1.Container[];
    installerBounds?: hostkeeper_release_v1.InstallerBounds;
    identity?: boolean;
    identityAccount?: string;
  }
  export interface Param { text?: string; integer?: string; float?: number | "NaN" | "Infinity" | "-Infinity"; flag?: boolean; }
  export interface Place { kind?: string; source?: string; path?: string; }
  export interface Port { host?: number; container?: number; loopbackOnly?: boolean; }
  export interface Repository {
    name?: string;
    kind?: string;
    uri?: string;
    suites?: string[];
    components?: string[];
    fingerprints?: string[];
    keyring?: string;
    access?: string;
    credentialFile?: string;
    descriptor?: string;
    baseUri?: string;
  }
  export interface StepBounds { prepare?: string; cutover?: string; verify?: string; rollback?: string; cleanup?: string; uninstall?: string; }
  export interface Target {
    installer?: string;
    locations?: hostkeeper_release_v1.Location[];
    digest?: string;
    bytes?: string;
    attestation?: hostkeeper_release_v1.Blob;
    manifest?: hostkeeper_release_v1.Manifest;
    repository?: string;
    packageVersion?: string;
  }
  export interface TargetList { entries?: hostkeeper_release_v1.Target[]; }
  export interface Volume { hostPath?: string; containerPath?: string; readOnly?: boolean; }
}
