# Hosting on AWS: the plan

One small server in its own AWS account, running the Crossfire C server behind the
Rust gateway. Security first; about US$20 a month.

## Shape

```
player (Crossfir3d client)
   │  TLS, port 13327 (the only open port)
   ▼
EC2 t4g.small, Amazon Linux 2023, own VPC, Elastic IP
   ├── gateway        user `gateway`, listens 0.0.0.0:13327
   │      │  plain TCP, this machine only
   │      ▼
   └── crossfire-server  user `crossfire`, port 13326, blocked from outside twice
          └── /srv/crossfire (players, accounts): separate encrypted EBS volume
```

## Security

| Risk | What stops it |
| --- | --- |
| Passwords read on the network | TLS only (gateway); Let's Encrypt certificate |
| Bad or hostile messages | Gateway: 22 known commands, size, rate, password-try and connection limits |
| Reaching the C server directly | Not in the security group, **and** a host firewall (nftables) drops port 13326 except from 127.0.0.1. (The C server listens on every address and has no setting for it.) |
| DM password guessing | Gateway blocks `dm`. A DM connects through an SSM port forward to 127.0.0.1:13326 (plain, never on the internet). |
| A break-in to the C server | Own AWS account (cannot reach dailybriefing). systemd sandbox per service: own user, no new privileges, read-only system, private /tmp, writes only to its data folder, no capabilities, memory limit. |
| Admin login | No SSH, no port 22. Admin through SSM Session Manager (IAM, logged). IMDSv2 only. |
| Unpatched software | `dnf-automatic` security updates daily; the gateway and C server rebuilt from pinned sources. |
| Build tools on the server | None: GitHub Actions builds both for ARM Linux, puts them in a private S3 bucket; the server can only read that bucket. |
| Lost player data | Encrypted EBS; daily snapshots kept 14 days (Data Lifecycle Manager). |
| Surprise bills | AWS Budget alarm at US$30 a month, email to the owner. |
| Telling the world | The C server's metaserver announcement turned off. |

## Steps

**A. The owner** (outside what I may do: billing and new accounts)
1. In the AWS console (current account), turn on **AWS Organizations**.
2. Create a member account, for example `crossfir3d`, with its own email address.
3. In the new account, buy the domain in **Route 53 > Registered domains**. Route 53 makes its hosted zone.
4. Tell me the account ID and the domain. I add a CLI profile that uses the
   `OrganizationAccountAccessRole` role.

**B. Infrastructure** (me, CloudFormation in `deploy/aws.yaml`, deployed with the AWS CLI):
VPC with one public subnet, security group (13327 only), Elastic IP, DNS record
`play.<domain>`, instance role (SSM, read the build bucket, write TXT records in the
hosted zone for the certificate), encrypted data volume, snapshot policy, budget alarm.

**C. Builds** (me): a GitHub Actions workflow builds the gateway and the pinned C server
(with its arch and maps) for `aarch64-linux`, and uploads them to the bucket.

**D. Install** (me, `deploy/install.sh` run through SSM): users, nftables rules, systemd
units with the sandbox settings, certbot with the Route 53 DNS plugin (renewal restarts
the gateway), server settings (no metaserver, port 13326), CloudWatch log shipping.

**E. Check** (me): play from the client at `play.<domain>:13327`; a port scan from
outside shows only 13327; the C server port refuses from outside; `dm` is refused; a
snapshot exists after a day.

**F. Later**: move C server parts to Rust behind the gateway.

## Cost (about, per month)

| Item | US$ |
| --- | --- |
| t4g.small | 12 |
| EBS 8 GB system + 10 GB data, snapshots | 3 |
| Elastic IP | 3.60 |
| Route 53 hosted zone | 0.50 |
| S3, CloudWatch, data out | 1-2 |
| Domain | about 15-25 a year |
