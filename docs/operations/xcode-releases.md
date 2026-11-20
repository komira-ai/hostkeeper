# Xcode releases

The coordinator reads the list of Xcode releases once a day (`--xcode-feed-url`; an empty value turns
the reading off) and keeps it. A failed read is a log line and another try the next day, never an alert.

## The alert `xcode_release_waiting`

The coordinator raises the warning `xcode_release_waiting` while the list holds a release (not a beta,
not a release candidate) newer than any release its Xcode store holds. The message names the release's
version and build and the macOS it requires, and the alert's `version` is the release's version. There
is one alert, for the newest such release. See it with `hk alerts list`.

It clears when the store holds that release or a newer one. What to do: push the release from a Mac
signed in to the Apple developer account with `hk xcode-push <version>`. Until the Xcode store exists,
the alert stands while the list holds a release newer than any held.
