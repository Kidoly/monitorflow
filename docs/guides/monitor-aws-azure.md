---
title: "How to monitor AWS and Azure virtual machines"
description: "Watch the EC2 instances of an AWS account and the virtual machines of an Azure subscription: their state, status checks, processor, network and disks, with read-only credentials, and tie each one to its server."
slug: monitor-aws-azure
lang: en
translation: surveiller-aws-azure
---

# How to monitor AWS and Azure virtual machines

A cloud account holds machines that nobody installed the agent on, and machines whose host fails without them knowing. MonitorFlow reads an AWS account or an Azure subscription through its API, every 5 minutes from the central server, with credentials that can only read. Each virtual machine shows its state, its status checks and its basic metrics, and is tied to the server you already monitor when it is one.

## Why it matters

- **A failed status check** means the provider sees the machine or its host broken (lost network, a hardware fault, a full or impaired volume), often before anything inside the machine notices.
- **A stopped machine** that should run, or a forgotten one that runs and costs, shows in one list per account.
- **Machines without the agent** are listed too, with a button to install it.

## AWS: make a read-only access key

MonitorFlow calls three actions, none of which changes anything. Make an IAM user for it, with only this policy:

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Sid": "MonitorFlowReadOnly",
      "Effect": "Allow",
      "Action": [
        "ec2:DescribeInstances",
        "ec2:DescribeInstanceStatus",
        "cloudwatch:GetMetricData"
      ],
      "Resource": "*"
    }
  ]
}
```

These actions take no resource, so `"Resource": "*"` is the narrowest AWS allows. Then, on the user, **Security credentials › Create access key** gives the access key ID (`AKIA…`) and its secret access key. Without `cloudwatch:GetMetricData`, the machines are still listed and the account says the metrics cannot be read.

CloudWatch bills `GetMetricData` by metric asked for: MonitorFlow asks five per running instance every 5 minutes (processor, network in and out, disk reads and writes), about 43,200 a month per instance. Status checks come from EC2 and cost nothing.

## Azure: make a service principal with the Reader role

The built-in **Reader** role on the subscription is enough. With the Azure CLI:

```
az ad sp create-for-rbac --name monitorflow-reader --role Reader --scopes /subscriptions/<subscription-id>
```

It prints `appId` (the application, or client, ID), `password` (the client secret) and `tenant`. To give less than Reader, make a custom role with only these actions and assign it on the subscription instead:

```
Microsoft.Compute/virtualMachines/read
Microsoft.Compute/virtualMachines/instanceView/read
Microsoft.Network/networkInterfaces/read
Microsoft.Insights/metrics/read
```

Without `networkInterfaces/read` the machines are listed without their network cards and addresses, so fewer are tied to their server.

## Add the account

In the fleet, **Add a device**: a name such as *AWS production*, then tick **It is a cloud account**. Pick the provider and enter what it asks:

| Provider | Fields |
| --- | --- |
| AWS | Access key ID, secret access key, regions (like `eu-west-3, us-east-1`) |
| Azure | Directory (tenant) ID, application (client) ID, client secret, subscription ID |

**Test the credentials**: the central server signs in once and says how many machines it sees, how many run, and whether metrics can be read. **Add the device**. There is no address to type and no ping: the account's page shows its machines within 5 minutes.

The secret is stored sealed, never shown again and never written in the logs or the audit log. On the device's **Details**, **Cloud account › Change…** changes the regions or the secret; a new access key or another tenant needs its own secret. Relays never get cloud credentials: the account is always read by the central server.

## What MonitorFlow checks

| Shown | AWS | Azure |
| --- | --- | --- |
| State | Running, stopped, pending | Running, stopped, deallocated |
| Zone and type | Availability zone, instance type | Region, size |
| Processor | CloudWatch `CPUUtilization` | Azure Monitor `Percentage CPU` |
| Network | `NetworkIn`, `NetworkOut` | `Network In Total`, `Network Out Total` |
| Disks | `EBSReadBytes`, `EBSWriteBytes` | `Disk Read Bytes`, `Disk Write Bytes` |
| Status checks | System, instance and attached volume checks | Azure's availability of the machine (`VmAvailabilityMetric`) |

Network and disks are shown per second, averaged over 5 minutes. Basic EC2 monitoring has a point every 5 minutes, so the newest value can be 10 minutes old. Stopped machines have no metrics and no status checks.

Alerts that come with it, changed on the device's page like any threshold:

| Alert | Default |
| --- | --- |
| The account cannot be read (credentials refused, a right missing, the API not answering) | Critical after 10 minutes, with the provider's own message |
| A status check fails | Critical after 10 minutes, per machine |

## Machines and the servers you already monitor

MonitorFlow ties each machine to the server or device of the same customer it is, as for VMware: on Azure by its hardware ID (the VM ID is the one the agent reports), on AWS by a network card's MAC address, then by an address or the host name. The machine's line names its server and how it was matched; **Not this one** and **Match automatically** correct it. A machine without the agent has **Install the agent**.

## When an alert fires

- **A status check fails** on AWS: a system check means the host; stop and start the instance (not reboot) to move it to another one. An instance check means the machine itself: look at its console output for a full disk or a network setting. On Azure, look at the machine's **Resource health**.
- **The account cannot be read**: the message says what AWS or Azure refused: an access key deleted or rotated, a client secret expired (they last two years at most), or a right taken away.
