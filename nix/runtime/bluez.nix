# BlueZ's daemon and tools for the RTL8821CS, trimmed to what controllers need:
# HID over classic Bluetooth and GATT, the management tools and the HCI monitor.
# nixpkgs' bluez pulls in Python, ELL, libical, ALSA and udev for the audio, mesh,
# OBEX, MIDI and NFC plugins (a ~190 MB closure); this one needs only GLib and D-Bus.
# A2DP and AVRCP are off until the audio path exists (docs/bluetooth.md).
{ bluez, stdenv, autoreconfHook, pkg-config, dbus, glib, readline }:
stdenv.mkDerivation {
  pname = "bluez-h700";
  inherit (bluez) version src patches;
  strictDeps = true;
  nativeBuildInputs = [ autoreconfHook pkg-config ];
  buildInputs = [ dbus glib readline ];
  # D-Bus' own pkg-config directories live in its read-only store path.
  configureFlags = [
    "--localstatedir=/var"
    "--with-dbusconfdir=${placeholder "out"}/share"
    "--with-dbussessionbusdir=${placeholder "out"}/share/dbus-1/services"
    "--with-dbussystembusdir=${placeholder "out"}/share/dbus-1/system-services"
  ] ++ map (feature: "--disable-${feature}") [
    "a2dp" "avrcp" "network" "bap" "bass" "mcp" "ccp" "vcp" "micp" "csip" "tmap" "gmap" "asha"
    "hfp" "obex" "cups" "udev" "systemd" "manpages"
  ];
  # `make install` would create /var/lib/bluetooth.
  installFlags = [ "statedir=$(TMPDIR)/var/lib/bluetooth" ];
  # btmgmt is built but not installed by upstream. The daemon reads its
  # configuration through the prefix it was built with, so point that at /etc.
  postInstall = ''
    install -Dm755 tools/btmgmt $out/bin/btmgmt
    rm -f $out/etc/bluetooth/*.conf
    for conf in main input; do ln -s /etc/bluetooth/$conf.conf $out/etc/bluetooth/$conf.conf; done
  '';
  enableParallelBuilding = true;
  doCheck = false;
  meta = { inherit (bluez.meta) description homepage license platforms; };
}
