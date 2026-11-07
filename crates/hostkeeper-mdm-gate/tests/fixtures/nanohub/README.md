# Recorded NanoHUB webhook events

`Authenticate.2.json` and `DeviceInformation.1.json` are the recorded events of NanoMDM v0.9.0's
webhook tests (`service/webhook/testdata/` of github.com/micromdm/nanomdm at tag `v0.9.0`), the
NanoMDM release NanoHUB 0.2.0's `go.mod` pins. NanoMDM's own test checks that its webhook sends
exactly these bytes for these check-in and result messages, so they are the format NanoHUB 0.2.0
posts. They are copied byte for byte but for one value: `created_at`, which NanoMDM's test sets to
Go's zero time (it replaces the clock with `time.Time{}`) and NanoHUB sets to the time of the event,
is a time of the event here. The gate does not read `created_at`.

The other events the tests post (`TokenUpdate`, `CheckOut`, results with a serial, an `Error`,
an `Idle`, a `SecurityInfo` answer) are built by `tests/support/mod.rs` with the same fields
NanoMDM v0.9.0's `service/webhook/service.go` sets for each topic.

NanoMDM's licence, under which the two files are copied:

> Copyright 2021 Jesse Peterson
>
> Permission is hereby granted, free of charge, to any person obtaining a copy of this software
> and associated documentation files (the "Software"), to deal in the Software without
> restriction, including without limitation the rights to use, copy, modify, merge, publish,
> distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the
> Software is furnished to do so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in all copies or
> substantial portions of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING
> BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
> NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
> DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
> OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
