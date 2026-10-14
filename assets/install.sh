#!/usr/bin/env bash
set -euo pipefail

# Determine install prefix: /usr/local for root, ~/.local for current user
if [ "$(id -u)" -eq 0 ]; then
  BIN_DIR="/usr/local/bin"
  APP_DIR="/usr/local/share/applications"
  ICON_DIR="/usr/local/share/icons/hicolor/512x512/apps"
else
  BIN_DIR="${HOME}/.local/bin"
  DATA_DIR="${XDG_DATA_HOME:-${HOME}/.local/share}"
  APP_DIR="${DATA_DIR}/applications"
  ICON_DIR="${DATA_DIR}/icons/hicolor/512x512/apps"
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

for resource in portalo portalo.desktop portalo.png; do
  if [ ! -f "${SCRIPT_DIR}/${resource}" ]; then
    echo "Error: missing ${SCRIPT_DIR}/${resource}" >&2
    exit 1
  fi
done

echo "Installing Portalo to ${BIN_DIR}..."
mkdir -p "${BIN_DIR}" "${APP_DIR}" "${ICON_DIR}"

install -m 755 "${SCRIPT_DIR}/portalo" "${BIN_DIR}/portalo"
exec_path="${BIN_DIR}/portalo"
# First escape the quoted Exec argument, then the Desktop Entry string.
exec_path="${exec_path//\\/\\\\}"
exec_path="${exec_path//\"/\\\"}"
exec_path="${exec_path//\$/\\\$}"
exec_path="${exec_path//\`/\\\`}"
exec_path="${exec_path//%/%%}"
exec_path="${exec_path//\\/\\\\}"
exec_path="${exec_path//$'\n'/\\n}"
exec_path="${exec_path//$'\r'/\\r}"
exec_path="${exec_path//$'\t'/\\t}"
while IFS= read -r line; do
  case "$line" in
    # env provides a fixed executable for launchers that check it before
    # expanding %% in arguments (notably GLib/GIO).
    Exec=*) printf 'Exec=/usr/bin/env -- "%s"\n' "$exec_path" ;;
    *) printf '%s\n' "$line" ;;
  esac
done < "${SCRIPT_DIR}/portalo.desktop" > "${APP_DIR}/portalo.desktop"
chmod 644 "${APP_DIR}/portalo.desktop"

install -m 644 "${SCRIPT_DIR}/portalo.png" "${ICON_DIR}/portalo.png"

# Update desktop and icon caches if tools are present
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "${APP_DIR}" 2>/dev/null || true

echo "Portalo installed successfully!"
echo "Executable: ${BIN_DIR}/portalo"
echo "Launch from application menu or execute 'portalo' in terminal."
