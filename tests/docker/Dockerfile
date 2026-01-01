FROM archlinux:latest

# Install packages that provide systemd units we want to test
RUN pacman -Syu --noconfirm && \
    pacman -S --noconfirm \
    nginx \
    valkey \
    openssh \
    cronie \
    dbus \
    && pacman -Scc --noconfirm

# Create runtime directories
RUN mkdir -p /run/sysd /var/log

# Copy sysd binaries
COPY sysd /usr/local/bin/sysd
COPY sysdctl /usr/local/bin/sysdctl

# Make binaries executable
RUN chmod +x /usr/local/bin/sysd /usr/local/bin/sysdctl

# Default command runs the test script
COPY run-tests.sh /run-tests.sh
RUN chmod +x /run-tests.sh

CMD ["/run-tests.sh"]
