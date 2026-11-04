#!/bin/bash
# Start a lab machine: install the mounted host key and authorized keys for
# this machine's role, then run sshd in the foreground.
#   ROLE=hub     authorizes the worker key for `git` (lane pushes and fetches)
#                and the hub's own dispatch key for `git` (the pump's clone)
#   ROLE=worker  authorizes the hub's dispatch key for `worker`
set -euo pipefail
K=/lab/keys
install -m 600 "$K/${ROLE}_host" /etc/ssh/ssh_host_ed25519_key
install -m 644 "$K/${ROLE}_host.pub" /etc/ssh/ssh_host_ed25519_key.pub
cat > /etc/ssh/sshd_config <<CONF
HostKey /etc/ssh/ssh_host_ed25519_key
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin no
AllowUsers git worker vivi
AuthorizedKeysFile .ssh/authorized_keys /etc/ssh/authorized_keys.d/%u
Subsystem sftp internal-sftp
CONF
key_for() { # key_for <user> <private key name>: the user's identity and known_hosts
  local home; home=$(getent passwd "$1" | cut -d: -f6)
  install -d -m 700 -o "$1" -g "$1" "$home/.ssh"
  install -m 600 -o "$1" -g "$1" "$K/$2" "$home/.ssh/id_ed25519"
  install -m 644 -o "$1" -g "$1" "$K/known_hosts" "$home/.ssh/known_hosts"
}
authorize() { # authorize <user> <public key name>...: root-owned, as on the hub
  local user=$1; shift
  for k in "$@"; do cat "$K/$k.pub"; done > "/etc/ssh/authorized_keys.d/$user"
  chmod 644 "/etc/ssh/authorized_keys.d/$user"
}
case "$ROLE" in
  hub)
    key_for vivi dispatch
    authorize git worker dispatch ;;
  worker)
    key_for worker worker
    authorize worker dispatch ;;
esac
exec /usr/sbin/sshd -D -e
