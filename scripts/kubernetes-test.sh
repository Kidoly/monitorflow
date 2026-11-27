#!/usr/bin/env bash
# Real test of the Kubernetes check (ADR-0183): starts k3s in a container,
# applies the read-only service account of deploy/kubernetes, adds a
# deployment whose image does not exist and a claim of a storage class
# that does not exist, checks what the account may do, runs the `kubernetes` test of
# crates/apps/tests/real.rs against the cluster's API, then removes the
# container. Needs Docker with privileged containers. Port 30643 on
# 127.0.0.1.
set -euo pipefail
cd "$(dirname "$0")/.."

name=mf-k8s-test
port=30643
docker rm -f "$name" >/dev/null 2>&1 || true
work=$(mktemp -d)

cleanup() {
    docker rm -f "$name" >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT

# Without a pod network: kube-proxy and flannel need iptables modules a
# test machine may lack, and the check reads the API, not the pods'
# traffic. The node then stays not ready and every pod pending, which
# the test reads as such.
docker run -d --name "$name" --privileged -p "127.0.0.1:$port:6443" \
    rancher/k3s:v1.31.4-k3s1 server --tls-san 127.0.0.1 \
    --disable traefik --disable metrics-server --disable-network-policy \
    --disable-kube-proxy --flannel-backend=none >/dev/null

kubectl() {
    docker exec -i "$name" kubectl "$@"
}

# Waits for `$@` to succeed, at most 3 minutes.
wait_for() {
    for _ in $(seq 90); do
        if "$@" >/dev/null 2>&1; then return 0; fi
        sleep 2
    done
    echo "timed out waiting for: $*" >&2
    return 1
}

wait_for sh -c "docker exec $name kubectl get nodes -o name | grep -q node/"
kubectl apply -f - <deploy/kubernetes/monitorflow-reader.yaml >/dev/null
wait_for kubectl -n monitorflow get secret monitorflow-reader-token -o jsonpath='{.data.token}'

kubectl apply -f - >/dev/null <<'YAML'
apiVersion: v1
kind: Namespace
metadata:
  name: mf-test
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: broken
  namespace: mf-test
spec:
  replicas: 1
  selector:
    matchLabels: {app: broken}
  template:
    metadata:
      labels: {app: broken}
    spec:
      containers:
        - name: main
          image: registry.invalid/monitorflow/does-not-exist:1
---
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: waiting
  namespace: mf-test
spec:
  storageClassName: does-not-exist
  accessModes: [ReadWriteOnce]
  resources:
    requests:
      storage: 1Gi
YAML

# The pod exists and waits for its image.
wait_for sh -c "docker exec $name kubectl -n mf-test get pods -o name | grep -q broken-"

token=$(kubectl -n monitorflow get secret monitorflow-reader-token -o jsonpath='{.data.token}' | base64 -d)
kubectl -n monitorflow get secret monitorflow-reader-token -o jsonpath='{.data.ca\.crt}' | base64 -d >"$work/ca.crt"

# The service account lists what the check reads, and nothing more.
reader=system:serviceaccount:monitorflow:monitorflow-reader
for allowed in "list nodes" "list pods" "list persistentvolumeclaims" \
    "list deployments.apps" "list statefulsets.apps" "list daemonsets.apps"; do
    # shellcheck disable=SC2086 # the verb and the resource
    kubectl auth can-i $allowed --as="$reader" -A >/dev/null
done
for refused in "delete deployments.apps" "create pods" "get secrets" \
    "create pods/exec" "patch nodes"; do
    # shellcheck disable=SC2086
    if kubectl auth can-i $refused --as="$reader" -A >/dev/null 2>&1; then
        echo "the read-only service account may $refused" >&2
        exit 1
    fi
done

MF_APPS_TEST_KUBERNETES="127.0.0.1:$port" \
    MF_APPS_TEST_KUBERNETES_TOKEN="$token" \
    MF_APPS_TEST_KUBERNETES_CA="$work/ca.crt" \
    cargo test -p monitorflow-apps --test real kubernetes -- --nocapture
