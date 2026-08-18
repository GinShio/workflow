#!/usr/bin/env fish
# -*- mode: fish; sh-shell: fish; coding: utf-8; indent-tabs-mode: nil; -*-

# Fish shell abbreviations and aliases configuration

# ============================================================================
# General aliases
# ============================================================================
alias clear "clear && echo -en \"\e[3J\""
alias ping "ping -c 8"
alias condaenv "source $HOME/.local/share/anaconda3/etc/fish/conf.d/conda.fish"
if type -q fdfind; and not type -q fd
    alias fd "fdfind"
end
if type -q batcat; and not type -q bat
    alias bat "batcat"
end

# ============================================================================
# Package Management Functions
# ============================================================================

# Debian/Ubuntu package management with apt
function __pkg_mgmt_debian
    # repository management
    abbr -a Par  "sudo add-apt-repository"
    abbr -a Plr  "add-apt-repository --list"
    abbr -a Pref "sudo -E apt update"
    abbr -a Prr  "sudo add-apt-repository --remove"
    abbr -a Pmr  "sudo apt edit-sources"
    abbr -a Pnr  "sudo apt edit-sources"
    # package management
    abbr -a Parm "sudo apt autoremove --purge"
    abbr -a Pin  "sudo -E apt install"
    abbr -a Prm  "sudo apt purge"
    abbr -a Psi  "sudo -E apt-src install"
    # update management
    abbr -a Pdup "sudo -E apt full-upgrade"
    abbr -a Plu  "apt list --upgradable"
    abbr -a Pup  "sudo -E apt upgrade"
    # querying
    abbr -a Pif  "apt show"
    abbr -a Pse  "apt search"
    abbr -a Pwp  "apt-file search"
    abbr -a Plog "grep -E 'Install|Remove|Upgrade' /var/log/apt/history.log | tail -50"
    abbr -a Pdl  "apt download"
    abbr -a Pli  "apt list --installed"
    abbr -a Plf  "dpkg -L"
    abbr -a Pri  "sudo -E apt reinstall"
    abbr -a Pdp  "apt depends"
    abbr -a Prd  "apt rdepends"
    abbr -a Ppc  "apt policy"
    abbr -a Psa  "sudo -E apt satisfy"
    abbr -a Plm  "apt-mark showmanual"
    abbr -a Pla  "apt-mark showauto"
    abbr -a Pcl  "apt changelog"
    # locking
    abbr -a Pal  "sudo apt-mark hold"
    abbr -a Pll  "apt-mark showhold"
    abbr -a Prl  "sudo apt-mark unhold"
    # utilities
    abbr -a Pcln "sudo apt autoclean && sudo apt autoremove"
    abbr -a Pinr "sudo -E apt install --install-recommends"
    abbr -a Pps  "sudo needrestart -b"
    abbr -a Pve  "sudo -E apt check"
end

# openSUSE package management with zypper
function __pkg_mgmt_opensuse
    # repository management
    abbr -a Par  "sudo zypper addrepo -fcg"
    abbr -a Plr  "zypper repos -Np"
    abbr -a Pref "sudo -E zypper refresh"
    abbr -a Prr  "sudo zypper removerepo"
    abbr -a Pmr  "sudo zypper modifyrepo"
    abbr -a Pnr  "sudo zypper renamerepo"
    # package management
    abbr -a Parm "sudo zypper remove --clean-deps"
    abbr -a Pin  "sudo -E zypper install"
    abbr -a Prm  "sudo zypper remove -u"
    abbr -a Psi  "sudo -E zypper source-install"
    # update management
    abbr -a Pdup "sudo -E zypper dist-upgrade"
    abbr -a Plu  "zypper list-updates"
    abbr -a Pup  "sudo -E zypper update"
    # querying
    abbr -a Pif  "zypper info"
    abbr -a Pse  "zypper search"
    abbr -a Pwp  "zypper search --provides"
    abbr -a Plog "sudo cut -d \"|\" -f 1-4 -s --output-delimiter \" | \" /var/log/zypp/history | tail -50"
    abbr -a Pdl  "zypper download"
    abbr -a Pli  "zypper packages --installed-only"
    abbr -a Plf  "rpm -ql"
    abbr -a Pri  "sudo -E zypper install --force"
    abbr -a Pdp  "zypper info --requires"
    abbr -a Prd  "zypper search --requires"
    abbr -a Ppc  "zypper search --details"
    abbr -a Psa  "sudo -E zypper install"
    abbr -a Plm  "zypper packages --installed-only --sort-by-name"
    abbr -a Pla  "zypper packages --unneeded"
    abbr -a Pch  "rpm -q --changelog"
    # locking
    abbr -a Pal  "sudo zypper addlock"
    abbr -a Pll  "zypper locks"
    abbr -a Prl  "sudo zypper removelock"
    abbr -a Pcll "sudo zypper cleanlocks"
    # utilities
    abbr -a Pcln "sudo zypper clean"
    abbr -a Pinr "sudo -E zypper install-new-recommends"
    abbr -a Pps  "zypper ps"
    abbr -a Pve  "sudo -E zypper verify"
end

# Fedora/CentOS/RHEL package management with dnf
function __pkg_mgmt_fedora
    # repository management
    abbr -a Par  "sudo dnf config-manager --add-repo"
    abbr -a Plr  "dnf repolist"
    abbr -a Pref "sudo -E dnf makecache"
    abbr -a Prr  "sudo dnf config-manager --set-disabled"
    abbr -a Pmr  "sudo dnf config-manager --set-enabled"
    abbr -a Pnr  "sudo vi /etc/yum.repos.d/"
    # package management
    abbr -a Parm "sudo dnf autoremove"
    abbr -a Pin  "sudo -E dnf install"
    abbr -a Prm  "sudo dnf remove"
    abbr -a Psi  "sudo -E dnf builddep"
    # update management
    abbr -a Pdup "sudo -E dnf distro-sync"
    abbr -a Plu  "dnf check-update"
    abbr -a Pup  "sudo -E dnf upgrade"
    # querying
    abbr -a Pif  "dnf info"
    abbr -a Pse  "dnf search"
    abbr -a Pwp  "dnf provides"
    abbr -a Plog "dnf history"
    abbr -a Pdl  "dnf download"
    abbr -a Pli  "dnf list --installed"
    abbr -a Plf  "rpm -ql"
    abbr -a Pri  "sudo -E dnf reinstall"
    abbr -a Pdp  "dnf repoquery --requires"
    abbr -a Prd  "dnf repoquery --whatrequires"
    abbr -a Ppc  "dnf list --showduplicates"
    abbr -a Psa  "sudo -E dnf install"
    abbr -a Plm  "dnf repoquery --userinstalled"
    abbr -a Pla  "dnf repoquery --unneeded"
    abbr -a Pch  "dnf repoquery --changelogs"
    # locking
    abbr -a Pal  "sudo dnf versionlock add"
    abbr -a Pll  "dnf versionlock list"
    abbr -a Prl  "sudo dnf versionlock delete"
    abbr -a Pcll "sudo dnf versionlock clear"
    # utilities
    abbr -a Pcln "sudo dnf clean all"
    abbr -a Pinr "sudo -E dnf install --setopt=install_weak_deps=True"
    abbr -a Pps  "sudo dnf needs-restarting"
    abbr -a Pve  "sudo -E dnf check"
end

# Alpine Linux package management with apk
function __pkg_mgmt_alpine
    # repository management (handled in /etc/apk/repositories)
    abbr -a Par  "sudo vi /etc/apk/repositories"
    abbr -a Plr  "cat /etc/apk/repositories"
    abbr -a Pref "sudo apk update"
    abbr -a Prr  "sudo vi /etc/apk/repositories"
    abbr -a Pmr  "sudo vi /etc/apk/repositories"
    abbr -a Pnr  "sudo vi /etc/apk/repositories"
    # package management
    abbr -a Parm "sudo apk del"
    abbr -a Pin  "sudo apk add"
    abbr -a Prm  "sudo apk del"
    abbr -a Psi  "sudo apk add --virtual .build-deps"
    # update management
    abbr -a Pdup "sudo apk upgrade --available"
    abbr -a Plu  "apk list --upgradable"
    abbr -a Pup  "sudo apk upgrade"
    # querying
    abbr -a Pif  "apk info"
    abbr -a Pse  "apk search"
    abbr -a Pwp  "apk info --who-owns"
    abbr -a Plog "cat /var/log/apk.log | tail -50"
    abbr -a Pdl  "apk fetch"
    abbr -a Pli  "apk list --installed"
    abbr -a Plf  "apk info -L"
    abbr -a Pri  "sudo apk fix"
    abbr -a Pdp  "apk info -R"
    abbr -a Prd  "apk info -r"
    abbr -a Ppc  "apk policy"
    abbr -a Psa  "sudo apk add"
    abbr -a Plm  "apk list --installed"
    abbr -a Pla  "apk info"
    abbr -a Pcl  "apk log"
    # locking
    abbr -a Pal  "sudo apk add --no-commit-hooks"
    abbr -a Pll  "cat /etc/apk/world"
    abbr -a Prl  "sudo apk del"
    abbr -a Pcll "echo 'N/A for apk'"
    # utilities
    abbr -a Pcln "sudo apk cache clean"
    abbr -a Pinr "sudo apk add"
    abbr -a Pps  "lsof | grep 'DEL.*lib'"
    abbr -a Pve  "sudo apk audit"
end

# NixOS package management with nix
function __pkg_mgmt_nixos
    # repository management (channels)
    abbr -a Par  "sudo nix-channel --add"
    abbr -a Plr  "nix-channel --list"
    abbr -a Pref "sudo nix-channel --update"
    abbr -a Prr  "sudo nix-channel --remove"
    abbr -a Pmr  "sudo nix-channel --add"
    abbr -a Pnr  "sudo nix-channel --add"
    # package management
    abbr -a Parm "nix-collect-garbage -d"
    abbr -a Pin  "nix-env -iA nixos."
    abbr -a Prm  "nix-env -e"
    abbr -a Psi  "nix-build"
    # update management (system-wide requires nixos-rebuild)
    abbr -a Pdup "sudo nixos-rebuild switch --upgrade"
    abbr -a Plu  "nix-env -u --dry-run"
    abbr -a Pup  "nix-env -u"
    # querying
    abbr -a Pif  "nix-env -qa --description"
    abbr -a Pse  "nix search nixpkgs"
    abbr -a Pwp  "nix-locate"
    abbr -a Plog "nix-env --list-generations"
    abbr -a Pdl  "nix-store --export"
    abbr -a Pli  "nix-env -q"
    abbr -a Plf  "nix-store -qR"
    abbr -a Pri  "nix-env -e && nix-env -i"
    abbr -a Pdp  "nix-store -q --references"
    abbr -a Prd  "nix-store -q --referrers"
    abbr -a Ppc  "nix-env -qa --attr-path"
    abbr -a Psa  "nix-env -iA nixos."
    abbr -a Plm  "nix-env -q"
    abbr -a Pla  "nix-store --gc --print-roots"
    abbr -a Pcl  "nix-env --list-generations"
    # locking (pinning generations)
    abbr -a Pal  "nix-env --set-flag keep true"
    abbr -a Pll  "nix-env --list-generations"
    abbr -a Prl  "nix-env --set-flag keep false"
    abbr -a Pcll "echo 'Use nix-env --delete-generations'"
    # utilities
    abbr -a Pcln "nix-collect-garbage"
    abbr -a Pinr "nix-env -iA nixos."
    abbr -a Pps  "nix-store --verify --check-contents"
    abbr -a Pve  "nix-store --verify"
end

# Gentoo package management with Portage (emerge)
function __pkg_mgmt_gentoo
    # repository management (overlays)
    abbr -a Par  "sudo eselect repository add"
    abbr -a Plr  "eselect repository list"
    abbr -a Pref "sudo emerge --sync"
    abbr -a Prr  "sudo eselect repository remove"
    abbr -a Pmr  "sudo eselect repository enable"
    abbr -a Pnr  "sudo vi /etc/portage/repos.conf/"
    # package management
    abbr -a Parm "sudo emerge --depclean"
    abbr -a Pin  "sudo emerge -av"
    abbr -a Prm  "sudo emerge -C"
    abbr -a Psi  "sudo emerge --fetchonly"
    # update management
    abbr -a Pdup "sudo emerge -avuDN @world"
    abbr -a Plu  "emerge -uDNp @world"
    abbr -a Pup  "sudo emerge -avuDN @world"
    # querying
    abbr -a Pif  "emerge -s"
    abbr -a Pse  "emerge -S"
    abbr -a Pwp  "equery belongs"
    abbr -a Plog "genlop -l"
    abbr -a Pdl  "sudo emerge --fetchonly"
    abbr -a Pli  "qlist -I"
    abbr -a Plf  "qlist"
    abbr -a Pri  "sudo emerge -av1"
    abbr -a Pdp  "equery depends"
    abbr -a Prd  "equery depends -a"
    abbr -a Ppc  "eix"
    abbr -a Psa  "sudo emerge -av"
    abbr -a Plm  "qlist -I"
    abbr -a Pla  "emerge -pc"
    abbr -a Pcl  "equery changes"
    # locking (masking/unmasking)
    abbr -a Pal  "echo 'Add to /etc/portage/package.mask'"
    abbr -a Pll  "cat /etc/portage/package.mask"
    abbr -a Prl  "echo 'Remove from /etc/portage/package.mask'"
    abbr -a Pcll "echo 'Clear /etc/portage/package.mask'"
    # utilities
    abbr -a Pcln "sudo eclean distfiles"
    abbr -a Pinr "sudo emerge -av"
    abbr -a Pps  "glsa-check -t all"
    abbr -a Pve  "sudo emerge -uDN @world --pretend"
end

# ============================================================================
# Package management comment documentation
# ============================================================================
# package management
#   - repository management
#     - ar         addrepo                  添加源
#     - lr         repos                    列出源
#     - ref        refresh                  刷新源
#     - rr         removerepo               移除源
#     - mr         modifyrepo               调整源
#     - nr         renamerepo               重命名源
#   - package management
#     - arm        autoremove               自动移除
#     - in         install                  安装
#     - rm         remove                   移除
#     - si         source-install           源码包安装
#   - update management
#     - dup        dist-upgrade             发行版升级
#     - lu         list-updates             列出需升级的包
#     - up         update                   更新
#   - querying
#     - if         info                     获取包信息
#     - se         search                   查询
#     - wp         what-provides            提供的包/文件
#     - log                                 历史记录
#     - dl         download                 下载包
#     - li         list-installed           列出已安装
#     - lf         list-files               列出包文件
#     - ri         reinstall                重新安装
#     - dp         depends                  查看依赖
#     - rd         rdepends                 查看反向依赖
#     - pc         policy                   版本策略
#     - sa         satisfy                  满足依赖
#     - lm         list-manual              手动安装的包
#     - la         list-auto                自动安装的包
#     - cl         changelog                更新日志
#   - locking
#     - al         addlock                  锁定
#     - ll         locks                    列出锁定
#     - rl         removelock               移除锁定
#     - cll        cleanlocks               清除锁定
#   - utilities
#     - cln        clean                    清除本地缓存
#     - inr        install-new-recommends   安装新的推荐
#     - ps         check-process            检测最近修改但仍在运行的程序
#     - ve         verify                   验证依赖

# ============================================================================
# Auto-detect distribution and setup package management aliases
# ============================================================================
switch (/bin/bash -c ". /etc/os-release; echo \"\${NAME:-\${DISTRIB_ID}} \${VERSION_ID:-\${DISTRIB_RELEASE}}\"")
    case 'CentOS*' 'Fedora*' 'Oracle*' 'openEuler*' 'Red Hat*' 'Rocky*' 'AlmaLinux*'
        __pkg_mgmt_fedora
    case 'Debian*' 'Ubuntu*' 'Kali*' 'Deepin*'
        __pkg_mgmt_debian
    case 'openSUSE*'
        __pkg_mgmt_opensuse
    case 'Alpine*'
        __pkg_mgmt_alpine
    case 'NixOS*'
        __pkg_mgmt_nixos
    case 'Gentoo*'
        __pkg_mgmt_gentoo
    case '*'
        echo "Unknown distribution. Package management aliases not configured."
end
