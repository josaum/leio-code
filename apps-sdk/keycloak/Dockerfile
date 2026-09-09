FROM quay.io/keycloak/keycloak:latest AS builder

ENV KC_DB=dev-file
ENV KC_CACHE=local
ENV KC_HTTP_ENABLED=true
ENV KC_PROXY_HEADERS=xforwarded
ENV KC_HEALTH_ENABLED=true
ENV KC_METRICS_ENABLED=true

RUN /opt/keycloak/bin/kc.sh build

FROM quay.io/keycloak/keycloak:latest

COPY --from=builder /opt/keycloak/ /opt/keycloak/
COPY --chown=keycloak:keycloak realm /opt/keycloak/local-realm
COPY --chown=keycloak:keycloak --chmod=755 entrypoint.sh /opt/keycloak/entrypoint.sh

ENTRYPOINT ["/opt/keycloak/entrypoint.sh"]
