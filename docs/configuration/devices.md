# Devices and USB

The `devices` key (see the [reference](reference.md)) gives a zone the GPU, audio, the microphone, the camera, screen sharing and USB. This page is about USB.

## USB devices (`usb`)

```toml
[zones.gadgets]
devices = ["gpu", "audio", "usb"]
usb     = ["abcd"]          # every device of vendor abcd; "abcd:1234" for one product
```

At each zone start, `appzones` reads `/sys/bus/usb/devices` and, for every
device whose `idVendor` (and `idProduct`, if given) matches, binds:

- its node `/dev/bus/usb/BBB/DDD` (from `busnum`/`devnum`, checked against
  the device's `dev` MAJ:MIN), for libusb;
- every `/dev/hidrawN` whose `/sys/class/hidraw/hidrawN/device` lies below
  that device, for hidapi and hidraw readers;
- the device's own sysfs directory (its realpath, read-only), plus
  `/sys/bus/usb` and, with a hidraw node, `/sys/class/hidraw` (listings
  for enumeration: their links to other devices dangle in the zone), and
  `/sys/dev/char`;
- udev's records of those nodes, `/run/udev/data/c189:N` and
  `c243:N` (read-only), which libusb and hidapi built with libudev read.

Never all of `/dev/bus/usb` or `/dev/hidraw*` (a YubiKey sits next to
them), and never `/dev/uinput`: it would let the zone type into the host
session. A zone without `usb` has no `/dev/bus/usb`, no hidraw node and no
`/sys/bus/usb`.

- **Bound at start.** A device plugged in later, or replugged (its device
  number changes), is not in the zone: `appzones restart ZONE`. With no
  matching device at start the zone warns (`appzones logs ZONE`) and starts
  without one.
- **Permissions are the host's.** The zone opens the nodes as you. A USB
  node is `root 0664` and a hidraw node `root 0600` unless a udev rule
  gives the seat user access, e.g. on NixOS:
  `services.udev.extraRules = ''SUBSYSTEMS=="usb", ATTRS{idVendor}=="abcd", TAG+="uaccess"'';`
- **No hotplug.** A device that arrives later has no node in the zone,
  whatever libusb's hotplug callback says (with `network` `internet` or
  `none` udev's netlink events do not even reach the zone's own network
  namespace).
