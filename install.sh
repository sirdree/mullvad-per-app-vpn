#!/usr/bin/env bash
# Installer / uninstaller for mullvad-per-app-vpn (unofficial, not affiliated with Mullvad).
#
#   curl -fsSL https://raw.githubusercontent.com/sirdree/mullvad-per-app-vpn/main/install.sh | bash
#   ./install.sh                  from a git checkout or the unpacked release bundle
#   ./install.sh --uninstall      remove everything it installed (your key is kept unless --purge)
#
# Everything lives in your home directory; the only thing that touches the system is the optional sudoers file.
set -euo pipefail

REPO="sirdree/mullvad-per-app-vpn"
NAME="mullvad-per-app-vpn"
PREFIX="${PREFIX:-$HOME/.local/share/$NAME}"
BINDIR="${BINDIR:-$HOME/.local/bin}"
SUDOERS_FILE="/etc/sudoers.d/$NAME"
CONF_DIR="${VPN_NS_CONF_DIR:-$HOME/.config/vpn-ns}"
BUNDLE="$NAME-linux-x86_64.tar.gz"

MODE=install SUDOERS=ask LAUNCHERS=yes BUILD_RUST=no PURGE=no VERSION=latest ASSUME=no
TMP=""

say() { printf '%s\n' "$*"; }
die() { printf 'install.sh: %s\n' "$*" >&2; exit 1; }
cleanup() { if [ -n "$TMP" ]; then rm -rf "$TMP"; fi; }
trap cleanup EXIT

usage() {
  cat <<EOF
Usage: install.sh [options]
  --uninstall        remove the installed files, menu entries, Desktop icons and the sudoers file
  --purge            with --uninstall: also delete $CONF_DIR (this deletes your stored Mullvad key)
  --sudoers          create the sudoers file without asking
  --no-sudoers       do not create the sudoers file
  --no-launchers     do not create menu entries / Desktop icons
  --build-rust       build the Rust window from source (needs cargo) when no binary is available
  --version vX.Y.Z   download that release instead of the latest one (only when not run from a checkout)
  --yes              answer yes to every question
Environment: PREFIX (default $PREFIX), BINDIR (default $BINDIR)
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --uninstall) MODE=uninstall;;
    --purge) PURGE=yes;;
    --sudoers) SUDOERS=yes;;
    --no-sudoers) SUDOERS=no;;
    --no-launchers) LAUNCHERS=no;;
    --build-rust) BUILD_RUST=yes;;
    --version) shift; [ $# -gt 0 ] || die "--version needs a value"; VERSION="$1";;
    --yes|-y) ASSUME=yes;;
    -h|--help) usage; exit 0;;
    *) usage; die "unknown option: $1";;
  esac
  shift
done

[ "$(id -u)" -ne 0 ] || die "run this as your normal user, not as root (it installs into your home directory)"
case "$PREFIX" in */"$NAME") ;; *) die "PREFIX must end in /$NAME (got: $PREFIX)";; esac

have_tty() { { : </dev/tty; } 2>/dev/null; }

ask() {   # ask "question [y/N]" default(y|n): returns 0 for yes
  local reply=""
  if [ "$ASSUME" = yes ]; then return 0; fi
  if have_tty; then
    printf '%s ' "$1" >/dev/tty
    read -r reply </dev/tty || reply=""
  fi
  case "${reply:-$2}" in y|Y|yes|YES) return 0;; *) return 1;; esac
}

real_cmd() { readlink -f "$(command -v "$1")"; }

# ------------------------------------------------------------------ uninstall
if [ "$MODE" = uninstall ]; then
  if [ -x "$PREFIX/vpn-ns" ]; then
    "$PREFIX/vpn-ns" down 2>&1 | sed 's/^/vpn-ns: /' || true
  fi
  apps_dir="${VPN_NS_DESKTOP_DIR:-$HOME/.local/share/applications}"
  for f in "$apps_dir"/vpn-*.desktop; do
    [ -e "$f" ] || continue
    if grep -q '^X-VPN-NS=app' "$f" || case "$f" in */vpn-picker-*.desktop) true;; *) false;; esac; then rm -f "$f"; say "removed $f"; fi
  done
  if [ -z "${VPN_NS_DESKTOP_DIR:-}" ]; then rm -f "$HOME/Desktop/Mullvad-VPN-rust.desktop" "$HOME/Desktop/Mullvad-VPN-python.desktop"; fi
  for l in vpn-ns vpn-ns-qt vpn-ns-gui; do
    if [ -L "$BINDIR/$l" ]; then rm -f "$BINDIR/$l"; fi
  done
  if [ -d "$PREFIX" ]; then rm -rf "$PREFIX"; say "removed $PREFIX"; fi
  if [ -e "$SUDOERS_FILE" ]; then
    say "removing $SUDOERS_FILE (sudo will ask for your password)"
    sudo rm -f "$SUDOERS_FILE"
  fi
  if [ -d /etc/netns/vpn ] && [ ! -e /run/netns/vpn ]; then sudo rm -rf /etc/netns/vpn; fi
  if [ "$PURGE" = yes ]; then
    if ask "Delete $CONF_DIR including your stored Mullvad key? [y/N]" n; then rm -rf "$CONF_DIR"; say "removed $CONF_DIR"; fi
  else
    say "kept $CONF_DIR (your key, server list and app list); use --purge to delete it"
  fi
  say "Done. The Mullvad device you registered stays in your Mullvad account until you remove it there."
  exit 0
fi

# ------------------------------------------------------------------ find the files to install
SELF="${BASH_SOURCE[0]:-}"
SRC=""
if [ -n "$SELF" ] && [ -f "$SELF" ]; then
  d="$(cd "$(dirname "$SELF")" && pwd)"
  if [ -f "$d/vpn-ns" ] && [ -f "$d/vpn-ns-qt" ]; then SRC="$d"; fi
fi
if [ -z "$SRC" ]; then
  [ "$(uname -m)" = x86_64 ] || die "the downloadable bundle is x86_64 only; clone https://github.com/$REPO and run ./install.sh there"
  command -v curl >/dev/null || die "curl is needed to download the release"
  command -v sha256sum >/dev/null || die "sha256sum is needed to verify the download"
  TMP="$(mktemp -d)"
  if [ -n "${MPAV_BASE_URL:-}" ]; then base="$MPAV_BASE_URL"   # testing only
  elif [ "$VERSION" = latest ]; then base="https://github.com/$REPO/releases/latest/download"
  else base="https://github.com/$REPO/releases/download/$VERSION"; fi
  say "Downloading $BUNDLE ($VERSION)..."
  curl -fsSL "$base/$BUNDLE" -o "$TMP/$BUNDLE" || die "download failed: $base/$BUNDLE"
  curl -fsSL "$base/$BUNDLE.sha256" -o "$TMP/$BUNDLE.sha256" || die "download failed: $base/$BUNDLE.sha256"
  (cd "$TMP" && sha256sum -c "$BUNDLE.sha256" >/dev/null) || die "checksum mismatch: the download is damaged or was changed, nothing was installed"
  say "Checksum OK."
  tar -xzf "$TMP/$BUNDLE" -C "$TMP"
  SRC="$TMP/$NAME"
  [ -f "$SRC/vpn-ns" ] || die "unexpected bundle layout"
fi

# ------------------------------------------------------------------ requirements
missing=()
for c in ip wg python3 curl setpriv sudo; do command -v "$c" >/dev/null || missing+=("$c"); done
if [ "${#missing[@]}" -gt 0 ]; then
  say "Missing programs: ${missing[*]}"
  say "  Debian/Ubuntu: sudo apt install iproute2 wireguard-tools python3 curl util-linux sudo"
  say "  Fedora:        sudo dnf install iproute wireguard-tools python3 curl util-linux sudo"
  say "  Arch:          sudo pacman -S iproute2 wireguard-tools python curl util-linux sudo"
  die "install those and run the installer again"
fi

# ------------------------------------------------------------------ copy the files
mkdir -p "$PREFIX" "$BINDIR"
install -m 0755 "$SRC/vpn-ns" "$PREFIX/vpn-ns"
install -m 0755 "$SRC/vpn-ns-qt" "$PREFIX/vpn-ns-qt"
install -m 0644 "$SRC/apps.conf" "$PREFIX/apps.conf"
rm -rf "$PREFIX/flags"
if [ -d "$SRC/flags" ]; then cp -r "$SRC/flags" "$PREFIX/flags"; else cp -r "$SRC/rust/flags" "$PREFIX/flags"; fi
for f in LICENSE README.md install.sh; do
  if [ -f "$SRC/$f" ]; then cp "$SRC/$f" "$PREFIX/$f"; fi
done

gui=""
for cand in "$SRC/vpn-ns-gui" "$SRC/rust/target/release/vpn-ns-gui"; do
  if [ -x "$cand" ]; then gui="$cand"; break; fi
done
if [ -z "$gui" ] && [ "$BUILD_RUST" = yes ]; then
  command -v cargo >/dev/null || die "--build-rust needs cargo (https://rustup.rs or: sudo apt install cargo)"
  say "Building the Rust window (a few minutes the first time)..."
  (cd "$SRC/rust" && CARGO_RESOLVER_INCOMPATIBLE_RUST_VERSIONS=fallback cargo build --release)
  gui="$SRC/rust/target/release/vpn-ns-gui"
fi
rm -f "$PREFIX/vpn-ns-gui"
if [ -n "$gui" ]; then
  install -m 0755 "$gui" "$PREFIX/vpn-ns-gui"
  if ldd "$PREFIX/vpn-ns-gui" 2>&1 | grep -q 'not found'; then
    say "The prebuilt Rust window does not run on this system (needs a newer glibc or a missing library):"
    ldd "$PREFIX/vpn-ns-gui" 2>&1 | grep 'not found' | sed 's/^/    /'
    say "  Build it yourself with ./install.sh --build-rust from a git checkout, or use the Python window."
    rm -f "$PREFIX/vpn-ns-gui"
  fi
else
  say "No Rust window installed (no prebuilt binary here). Use the Python window, or build with ./install.sh --build-rust."
fi

for l in vpn-ns vpn-ns-qt vpn-ns-gui; do
  if [ -e "$PREFIX/$l" ]; then ln -sf "$PREFIX/$l" "$BINDIR/$l"; fi
done
say "Installed into $PREFIX (commands linked in $BINDIR)."
case ":$PATH:" in *":$BINDIR:"*) ;; *) say "Note: $BINDIR is not in your PATH; the menu entries work anyway.";; esac

if ! python3 -I -c 'import PyQt6.QtWidgets' 2>/dev/null; then
  say "Note: the Python window needs PyQt6:  sudo apt install python3-pyqt6   (the Rust window does not)"
fi
if ! command -v kdialog >/dev/null && ! command -v zenity >/dev/null; then
  say "Note: no kdialog/zenity found: the key window's Browse button will not work (you can still type the path)."
fi

# ------------------------------------------------------------------ sudoers
IP="$(real_cmd ip)"; MKDIR="$(real_cmd mkdir)"; TEE="$(real_cmd tee)"
if sudo -n -k "$IP" netns list >/dev/null 2>&1; then
  say "Passwordless sudo already works for the network commands: no sudoers file needed."
elif [ "$SUDOERS" = no ]; then
  say "Skipped the sudoers file. The windows cannot type a password, so VPN actions will fail until it exists (see README)."
else
  do_sudoers=no
  if [ "$SUDOERS" = yes ] || [ "$ASSUME" = yes ]; then
    do_sudoers=yes
  elif have_tty; then
    say ""
    say "The VPN needs a few network commands as root (create a network namespace and a WireGuard interface)."
    say "A sudoers file can allow exactly those commands without a password for $(id -un)."
    say "Note: 'ip netns exec' can run any program as root, so this is effectively passwordless root for your user."
    if ask "Create $SUDOERS_FILE now? [y/N]" n; then do_sudoers=yes; fi
  else
    say "No terminal to ask: run ./install.sh --sudoers later to create the sudoers file."
  fi
  if [ "$do_sudoers" = yes ]; then
    t="$(mktemp)"
    {
      echo "# Installed by $NAME. Remove with: ./install.sh --uninstall (or delete this file)."
      echo "# Lets $(id -un) run the network-namespace commands of vpn-ns without a password (the GUI cannot type one)."
      echo "$(id -un) ALL=(root) NOPASSWD: $IP netns *, $IP link add wgvpn type wireguard, $IP link set wgvpn netns vpn, $MKDIR -p /etc/netns/vpn, $TEE /etc/netns/vpn/resolv.conf"
    } > "$t"
    if sudo visudo -cf "$t" >/dev/null; then
      sudo install -m 0440 -o root -g root "$t" "$SUDOERS_FILE"
      say "Created $SUDOERS_FILE"
    else
      say "The generated sudoers file did not validate, nothing was installed."
    fi
    rm -f "$t"
  fi
fi

# ------------------------------------------------------------------ menu entries
if [ "$LAUNCHERS" = yes ]; then
  "$PREFIX/vpn-ns" launchers >/dev/null
  say "Created menu entries (and Desktop icons for the window(s))."
fi

cat <<EOF

Next steps
  1. Open "Mullvad VPN" from your application menu (or run: $BINDIR/vpn-ns-qt).
  2. It asks for your key: in your Mullvad account create a WireGuard key, download the .conf, and import it in the window.
     One key works for every server.
  3. Click a server. Then start "Firefox (Mullvad VPN)" from the menu: only apps started that way use the VPN.
EOF
