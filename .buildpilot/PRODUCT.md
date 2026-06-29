# watsonx.data Easy Installer

<!-- Last verified: 2026-06-17 by [name/role] -->
<!-- Schema: agents/product-manager/product-schema.md -->

## What We Do [REQUIRED]

We turn the multi-day ordeal of standing up of a "self-managed" [IBM watsonx.data](https://www.ibm.com/docs/en/watsonxdata/premium/2.4.x?topic=installing-administering) into a single, dependable install with a minimal-memory footprint rust-based tool with a neat and simple UI with IBM carbon theme. Create a fresh OpenShift install on hyperscalers (IBM cloud / AWS / Azure / GCP) OR Point at an existing OpenShift cluster, answer questions (or leverage non-interactive mode), and we handle every prerequisite, every shared component, and every configuration step that today lives scattered across a dozen documentation pages. We exist for the developers and platform teams who need watsonx.data running — not a research project in reading IBM install guides. This tool should have multiple modules, each self-contained, and can be used in plug-n-play to construct this tool. This tool should be very very easy for users to help understand with clear tracking of steps end-to-end on the UI, provide clear and actionable errors, progress from one module to other seamlessly (stored from checkpoint) or resume from one to other step if failed in between. AWS creds are stored at `/Users/mrkr/.aws/credentials` file and IBM API key is at `/Users/mrkr/.ibm/IBM_CLOUD_API_KEY`

## Who Uses It [REQUIRED]

<!-- TODO: confirm these personas with two real early users and their actual jobs-to-be-done. -->
- **Platform admin**: A platform admin is responsible for setting up the cluster.

### Persona 1: Platform Engineer Standing Up the Stack
- **Who**: Platform or infrastructure engineer at an enterprise that has already invested in OpenShift and wants to adopt watsonx.data. Comfortable with clusters and command lines, new to this specific product's install path.
- **Goals**: Get a working watsonx.data instance on an existing cluster without becoming an expert in IBM's install documentation first.
- **Pain Points**: The install is spread across many documentation topics, each with its own prerequisites, variable files, and ordering rules. One missed step early surfaces as a cryptic failure much later.
- **Usage Pattern**: Bursty — heavy use during an initial install or an environment rebuild, then quiet. May run it across several clusters (dev, staging, prod).
- **Technical Level**: Technical / Developer

### Persona 2: Developer Spinning Up an Evaluation Environment
- **Who**: Developer or data engineer tasked with evaluating watsonx.data for a team, with a cluster handed to them and a deadline.
- **Goals**: Get to a running instance fast enough to actually try the product, not spend the evaluation window on setup.
- **Pain Points**: The setup cost is high enough that evaluation stalls before it starts; collecting the required cluster information and populating configuration by hand is error-prone.
- **Usage Pattern**: One-time or occasional, under time pressure.
- **Technical Level**: Technical

## Problems We Solve [REQUIRED]

1. **Installing watsonx.data is punishingly hard today**: A developer has to march through many separate documentation topics in the right order, collect cluster details by hand, and populate configuration files correctly before anything works. We collapse that marathon into one guided, repeatable install.
2. **Prerequisites are easy to miss and expensive to debug**: Skipped or misordered setup steps fail late and obscurely. We collect the required information up front and run the prerequisites and shared-component setup for you, in the right order.
3. **Configuration is hand-assembled and fragile**: Today the configuration values are typed into scripts by hand, where a single wrong value derails the install. We capture the inputs once and generate the configuration so it's right the first time.

## Key Features [REQUIRED]

<!-- TODO: this product is at the idea/early stage. Confirm and expand this feature set as the installer is built; today these reflect the intended scope. -->

| Feature | Description | User Benefit |
|---------|-------------|--------------|
| Guided install against an existing cluster | You provide your OpenShift cluster details; we run the install end to end | A working watsonx.data instance without reading the install guides first |
| Prerequisite collection and setup | We gather the required cluster information and stand up the shared components needed before install | No missed steps failing late in the process |
| Configuration generation | We turn your answers into the configuration the install needs | No hand-typed values, no single-character mistakes derailing the run |
| Repeatable runs across environments | The same guided flow works across dev, staging, and production clusters | Consistent installs, not a one-off heroic effort each time |

## Product Principles [REQUIRED]

<!-- TODO: founder/PM to confirm the real constraints that should override convenience. Drafted below from the product's stated intent — replace with the team's actual trade-offs. -->

1. **Done beats documented**: We'd rather perform a step for the user than explain how to do it. Every place we point a user back at a manual is a place we've failed.
2. **Fail early, fail clearly**: We'd rather stop up front with a specific, fixable message than let a missed prerequisite surface as a cryptic failure an hour into the install.
3. **Meet the cluster they already have**: We adapt to an existing OpenShift cluster rather than demanding a pristine one. The user's environment is the starting point, not a precondition.

## Differentiators [RECOMMENDED]

<!-- TODO: confirm with the founder. Drafted from intent. -->

1. **One install path, not a documentation trail**: The product's whole reason to exist is replacing a sprawl of manual topics with a single dependable run.

## Positioning [RECOMMENDED]

- **Market segment**: [TODO: confirm]
- **Orientation**: Vertical — IBM watsonx.data on OpenShift
- **Deployment**: Runs against the customer's own OpenShift cluster
- **Target company size**: [TODO: confirm — likely mid-market and enterprise, where OpenShift and watsonx.data adoption live]

## How It Works [OPTIONAL]

```
You provide your existing OpenShift cluster details
   → we collect the required information and check prerequisites
   → we set up the shared components watsonx.data depends on
   → we generate the configuration from your inputs
   → we run the install
   → you get a working watsonx.data instance
```

## Known Limitations [OPTIONAL]

| Limitation | Reason | Future Plan |
|-----------|--------|-------------|
| We don't provision the OpenShift cluster itself | We install onto an existing cluster; cluster creation is the customer's responsibility | TBD |
| Scoped to IBM watsonx.data | The install logic is specific to this product, not a general installer | An option to install additional services later on as we expand the scope of this project |

## Integrations [OPTIONAL]

| Integration | Purpose | Status |
|------------|---------|--------|
| IBM watsonx.data | The product being installed | Active |
| Red Hat OpenShift | The cluster the install targets | Active |

## Glossary [OPTIONAL]

| Term | Definition |
|------|-----------|
| watsonx.data | IBM's data store / lakehouse product that this tool installs |
| OpenShift | The Red Hat Kubernetes platform the install runs against |
| Prerequisites | The setup steps and shared components that must be in place before watsonx.data can be installed |
