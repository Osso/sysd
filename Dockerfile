FROM archlinux:latest

# For testing zombie reaping and spawning processes
RUN pacman -Sy --noconfirm procps-ng

# sysd binary mounted from host
CMD ["/usr/bin/sysd", "-f"]
