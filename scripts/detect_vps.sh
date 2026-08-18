#!/bin/sh
#
# detect_vps.sh
# Detects the current VPS/Cloud platform.
# Returns a lowercase string representing the platform (e.g., aws, gcp, alibaba, etc.)
# POSIX compliant.

detect_vps() {
    _vendor=""
    _product=""

    if [ -r "/sys/class/dmi/id/sys_vendor" ]; then
        _vendor=$(cat /sys/class/dmi/id/sys_vendor 2>/dev/null)
    fi

    if [ -r "/sys/class/dmi/id/product_name" ]; then
        _product=$(cat /sys/class/dmi/id/product_name 2>/dev/null)
    fi

    _dmi_info="$_vendor $_product"

    case "$_dmi_info" in
        *Amazon*|*EC2*|*amazon*) echo "aws" ;;
        *Google*)                echo "gcp" ;;
        *Microsoft*|*Azure*)     echo "azure" ;;
        *DigitalOcean*)          echo "digitalocean" ;;
        *Linode*)                echo "linode" ;;
        *Vultr*|*Choopa*)        echo "vultr" ;;
        *Oracle*)                echo "oracle" ;;
        *Alibaba*|*Aliyun*)      echo "alibaba" ;;
        *Tencent*)               echo "tencent" ;;
        *UpCloud*)               echo "upcloud" ;;
        *Hetzner*)               echo "hetzner" ;;
        *QEMU*|*KVM*|*pc-i440fx*|*pc-q35*) echo "kvm" ;;
        *VMware*)                echo "vmware" ;;
        *VirtualBox*)            echo "virtualbox" ;;
        *Xen*)                   echo "xen" ;;
        *)
            # Fallbacks for container environments or generic virtualization
            if [ -f "/.dockerenv" ]; then
                echo "docker"
            elif grep -q "lxc" /proc/1/environ 2>/dev/null || grep -q "lxc" /proc/1/cgroup 2>/dev/null; then
                echo "lxc"
            elif grep -q "QEMU" /proc/cpuinfo 2>/dev/null; then
                echo "kvm"
            elif command -v systemd-detect-virt >/dev/null 2>&1; then
                _virt=$(systemd-detect-virt 2>/dev/null)
                if [ -n "$_virt" ] && [ "$_virt" != "none" ]; then
                    echo "$_virt"
                else
                    echo "unknown"
                fi
            else
                echo "unknown"
            fi
            ;;
    esac
}
