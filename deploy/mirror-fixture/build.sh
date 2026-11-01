#!/bin/sh
# Builds /srv: one package for all architectures in an apt repository signed
# by a throwaway key, with a component that holds nothing, and one noarch
# package in a dnf repository with a group. /srv/next is the same upstream
# with a new version of the package, which replaced the old one: the smoke
# test swaps it in to prove that retention frees what upstream dropped. Runs
# when the fixture image is built.
set -eu
export GNUPGHOME=/tmp/gnupg
mkdir -m 700 "$GNUPGHOME"
gpg --batch --quiet --pinentry-mode loopback --passphrase '' \
    --quick-generate-key 'MonitorFlow fixture upstream' rsa2048 sign never
gpg --batch --export >/srv/upstream-key.gpg

# build_apt <version> <directory>
build_apt() {
    pkg=$(mktemp -d)
    mkdir -p "$pkg/DEBIAN" "$pkg/usr/bin"
    printf '#!/bin/sh\necho fixture\n' >"$pkg/usr/bin/mf-fixture"
    chmod 755 "$pkg/usr/bin/mf-fixture"
    cat >"$pkg/DEBIAN/control" <<CONTROL
Package: mf-fixture
Version: $1
Architecture: all
Maintainer: MonitorFlow <noreply@example.com>
Description: Fixture of the mirror smoke test
CONTROL
    apt=$2
    mkdir -p "$apt/pool" "$apt/dists/fixture/main/binary-amd64" "$apt/dists/fixture/empty/binary-amd64"
    dpkg-deb --root-owner-group --build "$pkg" "$apt/pool/mf-fixture_$1_all.deb" >/dev/null
    cd "$apt"
    apt-ftparchive packages pool | gzip -9n >dists/fixture/main/binary-amd64/Packages.gz
    gzip -9n </dev/null >dists/fixture/empty/binary-amd64/Packages.gz
    cd dists/fixture
    apt-ftparchive -o APT::FTPArchive::Release::Suite=fixture -o APT::FTPArchive::Release::Codename=fixture \
        -o APT::FTPArchive::Release::Architectures=amd64 -o 'APT::FTPArchive::Release::Components=main empty' \
        release . >/tmp/Release
    mv /tmp/Release Release
    gpg --batch --quiet --pinentry-mode loopback --passphrase '' --clearsign -o InRelease Release
}

# build_dnf <version> <directory>
build_dnf() {
    rm -rf /tmp/rpm
    mkdir -p /tmp/rpm "$2/Packages"
    cat >/tmp/rpm/fixture.spec <<SPEC
Name: mf-fixture
Version: $1
Release: 1
Summary: Fixture of the mirror smoke test
License: MIT
BuildArch: noarch
%description
Fixture of the mirror smoke test.
%install
mkdir -p %{buildroot}/usr/bin
printf '#!/bin/sh\necho fixture\n' >%{buildroot}/usr/bin/mf-fixture
chmod 755 %{buildroot}/usr/bin/mf-fixture
%files
/usr/bin/mf-fixture
SPEC
    rpmbuild --quiet -bb --define "_topdir /tmp/rpm" --define "_rpmdir $2/Packages" \
        --define '_build_name_fmt %%{NAME}-%%{VERSION}-%%{RELEASE}.%%{ARCH}.rpm' /tmp/rpm/fixture.spec
    createrepo_c --quiet "$2"
    cat >/tmp/comps.xml <<COMPS
<?xml version="1.0"?>
<comps><group><id>fixture</id><name>Fixture</name><description>Fixture</description>
<packagelist><packagereq>mf-fixture</packagereq></packagelist></group></comps>
COMPS
    modifyrepo_c --mdtype=group /tmp/comps.xml "$2/repodata" >/dev/null
}

build_apt 1.0 /srv/apt
build_dnf 1.0 /srv/dnf
build_apt 1.1 /srv/next/apt
build_dnf 1.1 /srv/next/dnf
