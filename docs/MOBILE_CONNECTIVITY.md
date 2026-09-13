# Mobile connectivity

The mobile status bar, shade header, and lockscreen use live Wi-Fi access-point
strength from the selected network backend and cellular signal quality from
ModemManager. Cellular bars reflect network registration and measured quality;
an exclamation mark indicates that the modem has no connected data bearer.
Registration alone does not indicate a data connection. Wi-Fi is connected only
when its NetworkManager device is activated, even if another transport is online.

The mobile shade's Mobile data card changes NetworkManager's `WwanEnabled`
property. NetworkManager retains ownership of profiles, APNs, autoconnect, and
radio policy. Enabling the switch relies on a configured mobile profile to
establish data service; the switch does not create or replace carrier profiles.
Disabling WWAN can also disable other services supplied by the modem's radio.
Both NetworkManager and ModemManager must be available to use this control.
Distribution D-Bus and polkit policies remain authoritative.

On the mobile lockscreen, a SIM requesting its primary PIN presents a separate
SIM unlock prompt. The PIN is obscured, restricted to 4–8 digits, cleared on
submission and disposal, and never saved or automatically retried. Remaining
attempts come from ModemManager. Primary PUK and other blocking locks are reported without
submitting a PIN. PIN2 and PUK2 protect supplementary SIM functions and do not
keep the normal-service unlock prompt open. The user can defer SIM unlock and continue device authentication;
unlocking the SIM never authenticates the desktop session. The prompt lives above
the device-lock transition, so fingerprint or password authentication cannot
dismiss it. It retains keyboard and pointer capture until SIM unlock or explicit
deferral.

Service discovery uses ObjectManager and property/owner-change signals, including
modem removal and daemon restarts. If several modems exist, a PIN-locked modem is
selected first, then a connected or registered modem. This is a single-modem
mobile control, not a multi-SIM management interface.

Nonvisual integration tests run against a private D-Bus server:

```sh
tools/denial-pc flutter-test test/services/mobile_network_service_test.dart test/widgets/sim_pin_panel_test.dart
```

Visual and on-device validation are user-owned. These tests do not send commands
to the system modem, change system networking, or create live shell UI events.

Protocol references:
- [ModemManager modem interface](https://github.com/linux-mobile-broadband/ModemManager/blob/main/introspection/org.freedesktop.ModemManager1.Modem.xml)
- [ModemManager SIM interface](https://github.com/linux-mobile-broadband/ModemManager/blob/main/introspection/org.freedesktop.ModemManager1.Sim.xml)
- [NetworkManager manager interface](https://networkmanager.pages.freedesktop.org/NetworkManager/NetworkManager/gdbus-org.freedesktop.NetworkManager.html)
