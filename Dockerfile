ARG BASE_IMG="alpine:3.19"
ARG BUILD_IMG="rust:1-alpine3.19"
ARG APP_NAME="rss_checker"

FROM $BUILD_IMG as builder
WORKDIR /usr/src/${APP_NAME}

COPY . .
RUN cargo install --path .

FROM $BASEIMG
LABEL maintainer="Nate Catelli <ncatelli@packetfire.org>"
LABEL description="Container for rss_checker"

ARG SERVICE_USER="service"
ARG APP_NAME="rss_checker"

RUN addgroup ${SERVICE_USER} \
    && adduser -D -G ${SERVICE_USER} ${SERVICE_USER}

COPY --from=builder /usr/local/cargo/bin/${APP_NAME} /opt/${APP_NAME}/bin/${APP_NAME}

RUN mkdir -p /opt/${APP_NAME}/.${APP_NAME}/cache \
    && chown ${SERVICE_USER}:${SERVICE_USER}  /opt/${APP_NAME}/.${APP_NAME}/cache \
    && chown ${SERVICE_USER}:${SERVICE_USER} /opt/${APP_NAME}/bin/${APP_NAME} \
    && chmod +x /opt/${APP_NAME}/bin/${APP_NAME}

VOLUME /opt/${APP_NAME}/.${APP_NAME}/cache

WORKDIR "/opt/${APP_NAME}/"
USER ${SERVICE_USER}

ENTRYPOINT [ "/opt/rss_checker/bin/rss_checker" ]
CMD [ "-h" ]