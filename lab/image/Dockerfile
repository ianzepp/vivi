# One machine image for the lab: the hub and every worker start from it.
# The vivi binary, the stand-in agent and the keys are mounted at run time.
# The hub's layout follows the real hub: projects under /srv/vivi/projects
# (owned by vivi), lanes under /srv/vivi/lanes (vivi:lanes, setgid), served
# over SSH by the git-shell user `git`, whose home is the lanes directory.
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
      bash ca-certificates coreutils git lsof openssh-client openssh-server \
      perl procps python3 sqlite3 tar tini \
  && rm -rf /var/lib/apt/lists/*
RUN groupadd lanes \
  && useradd -m -s /bin/bash -G lanes vivi \
  && useradd -M -d /srv/vivi/lanes -s /usr/bin/git-shell -G lanes git \
  && useradd -m -s /bin/bash worker \
  && for u in vivi git worker; do usermod -p '*' "$u"; done \
  && mkdir -p /run/sshd /srv/vivi/projects /srv/vivi/lanes /etc/ssh/authorized_keys.d \
  && chown vivi:vivi /srv/vivi/projects \
  && chown vivi:lanes /srv/vivi/lanes && chmod 2770 /srv/vivi/lanes \
  && git config --system safe.directory '*' \
  && echo 'umask 002' >> /home/vivi/.profile
COPY machine.sh /usr/local/sbin/machine.sh
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/sbin/machine.sh"]
