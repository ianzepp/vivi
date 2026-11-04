#!/bin/bash
# start-service.sh (inside the hub, as root): run the pump supervisor as vivi,
# the way vivi-pump.service does on the real hub, logging to ~vivi/service.log.
exec runuser -u vivi -- env HOME=/home/vivi USER=vivi bash -c \
  'cd / && umask 002 && exec /usr/local/bin/vivi pump service run >> /home/vivi/service.log 2>&1'
