FROM archlinux:latest

# Install packages matching the QEMU Arch test
RUN pacman -Syu --noconfirm && \
    pacman -S --noconfirm \
    base \
    openssh \
    dbus-broker \
    polkit \
    iproute2 \
    procps-ng \
    util-linux \
    nginx \
    valkey \
    cronie \
    && pacman -Scc --noconfirm

# Create runtime directories
RUN mkdir -p /run/sysd /run/dbus /var/log

# Generate machine-id (required by dbus-broker)
RUN systemd-machine-id-setup || uuidgen | tr -d '-' > /etc/machine-id

# Copy sysd binaries
COPY sysd /usr/bin/sysd
COPY sysdctl /usr/bin/sysdctl
COPY sysd-executor /usr/bin/sysd-executor

# Make binaries executable
RUN chmod +x /usr/bin/sysd /usr/bin/sysdctl /usr/bin/sysd-executor

# Enable services via symlinks (matches QEMU test setup)
RUN mkdir -p /etc/systemd/system/multi-user.target.wants \
    /etc/systemd/system/sockets.target.wants

# Enable dbus-broker
RUN ln -sf /usr/lib/systemd/system/dbus-broker.service /etc/systemd/system/multi-user.target.wants/ && \
    ln -sf /usr/lib/systemd/system/dbus.socket /etc/systemd/system/sockets.target.wants/

# Set default target
RUN ln -sf /usr/lib/systemd/system/multi-user.target /etc/systemd/system/default.target

# Copy test script
COPY test-arch.sh /test-arch.sh
RUN chmod +x /test-arch.sh

# sysd as PID 1 - use exec form to ensure it's actually PID 1
ENTRYPOINT ["/usr/bin/sysd", "--foreground"]
