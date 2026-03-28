FROM rust:1.94

USER root

RUN apt-get update && \
    apt-get install -y git && \
    rm -rf /var/lib/apt/lists/*

ENV GIT_AUTHOR_NAME="Batstone Agent"
ENV GIT_AUTHOR_EMAIL="batstone@localhost"
ENV GIT_COMMITTER_NAME="Batstone Agent"
ENV GIT_COMMITTER_EMAIL="batstone@localhost"

WORKDIR /workspace
