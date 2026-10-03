# OXO High Level Design (the speclet)

This is the original high-level specification of OXO, kept as written. It
was the project's `README.md` until the repository was prepared for public
release; other documents that say "the README" mean this file. The
[architecture design](2026-10-01-oxo-architecture-design.md) is the source of
truth: where the two differ on execution details, the architecture document
governs.

# Problem

XEarthLayer is a streaming orthoscenery provider for the X-Plane flgiht
simulator. XEarthLayer depends upon regional scenery packages in order to
provide the correct orthographic textures and mesh for the entire globe. These
resources are provided by Ortho4XP.

Due to the scale of the globe, producing orthographic scenery that covers the
entire planet takes many weeks, and requires a lot of manual orchestration tasks
to ensure all of the tiles are processed and then packaged effectively, while
ensuring that each machine producing tiles has enough disk space and memory
available to successfully complete the task. This requires a significant
investement of time from the operator while having multiple failure modes due to
memory and disk pressure, network failures, operating system updates and other
factors beyond the direct control of the manual operator.

This project will provide a control plane for this task, to automate the work
for producing XEarthLayer scenery packages, using existing tools;
ortho4xp and the xearthlayer-pubisher binaries.

# High Level Design

The high level concept for the design of this system revolves around three
phases of scenery package creation;

1. **Regional Scenery Package specification** - defining the geographic region, 
   providing the name and metadata about the region, and parameters for
   compilation (where should files go, etc.)
2. **Regional Scenery Production** - Actual work to create the tiles for the
   scenery itself. This is where `Ortho4XP` produces the actual Ortho tiles and
   the associated overlays. The production work should be done at the tile level
   atomically, that is to say that the work to produce a 1x1 degree tile include
   overlays is completed as a single-shot exeuction in isolation. Succcessive
   atomic work (or tasks) is completed in order to provide tiles for a region.
3. **Regional Scenery Package compilation** - Once all of the required tiles for
   a specific region have been completed successfully to specification, the
   tiles are compiled into the final regional scenery package for publication
   for XEarthLayer using the `xearthlayer-publish` tools.

## Regional Scenery Package Specification

The specification of scenery packages defines all of the necessary details
needed for an orchestrator to manage the lifecyle of work required to complete
the scenery package.

A regional scenery package needs to fulfill the requirementss of the regional
scenery package and associated library as defined in the main xearthlayer.app
specification.

In summary, regional scenery package contains a collection of one or more 1x1
degree orthographic tiles that are compatible with X-Plane. A region is usually
part or all of a continent, such as North Amercia (NA) or Oceania (OC).

The definition of regions area is controlled by specifying each and every 1x1
degree tile. Therefore a region is an enumeration / collection of 1x1 degree
tiles. Each of the tiles defined in the scenery package specifciation will
result in one piece of atomic work to process. Beyond the specification of the
geographic area, the other configuration parameters for a regional scenery
package will include;
- the parameters that Ortho4XP requires to complete the package (Ortho4XP.cfg)
- file system specification on where to fine xplane global/demo scenery
- where to place the completed resoures, ortho and overlay tiles
- failure policies for retries and alerting should a tile fail to process

Once the regional scenery package specification is completed and validated, it
can be submitted to the production phase for processing.

## Regional Scenery Production

Production of the regional scenery package requires two distinct components. The
first is the plan for the work. Using the specification provided by the
specification phase, the production phase needs to atomize the work into
individual tasks that can be processed. The second component is the work to
produce the tiles themselves, which should be a single task that any capable
worker can pick up, process the specification for the tile provided, return the
artifacts produced to the specified location and exit cleanly, prepare for a new
task.

At a high level, the production phase should start by splitting the
specification into _N_ tasks, which each task representing the work to produce a
single 1x1 tile (including the overlays optionally). The task can then be
committed to by a separate process that understands how to complete the task
successfully.

The work itself will be completed by Ortho4XP, likely running in a container
that lives for the lifecyle of the task itself before terminating. The runtime
for the container is not decided, but the design of the system should be able to
support simpler container runtimes such as Podman, as well as bigger more
sophisticated kubernetes fleets. Kubernetes is not a requirement up front, but
longer term a first class k8s operator is a reasonable goal for this project.

The production step needs to be able to maange and orchestrate the dependencies
for the work to be compeleted sucessfully, which includes tasks such as;

- Ensure the configuration provided is valid
- The configuration resources defined are reachable and staged effectively
- stage file system mounts to ensure the input of global scenery and output or
  completed tile resources can be completed successfully
- observability and telemetry is being exported to any defined outputs

The lifecyle of a particular container is largely expected to only live for the
length of each 1x1 task that is being completed. However, there are tradeoffs
with this design and there may be usecases for having longer lived containers
where the orchestration system has less control over the process management of a
system - i.e. K8s can control spec scale as needed, podman is an atomic runtime
that would need additional controllers to manage process scaling / lifecyle.

As a concrete implementation, the first usage of this system will be on the
authors home network, with 3-4 nodes, each capable of running 4-8 containers
concurrently each. These nodes are not part of a k8s cluster, so they will be
running local containers that should be able to start and automatically connect
to the control plane in order to do work. The containers should only know how to
do work provided to them, so there needs to be a controller on each node to
coordinate the work itself per node. This needs to be factored into the design
of this system, but the design should be compatible / interchangable with a k8s
operator model for future cloud based processing.

## Regional Scenery Package compilition

The final stage of regional scenery package production is compiling the final
XEarthLayer regional scenery package using the `xearthlayer-publish` tools
provided with the project. This part of the process should happen on a single
node that has access to the working xearthlayer scenery package library.

The compilation process should only happen once all of the required tasks for a
regional scenery package have completed successfully and constitute the final
stage of this process.

# Non-goals

- Create a bespoke distributed compute platform in order to fulfill these
  requirements. This project should use existing open source frameworks in order
  to deliver on the requirements.
- Create a bespoke job or task management system in order to fulfill these
  requirements. Simiar to above.
- Create a bespoke ortho tile processing system. Ortho4XP works fine for this
  task.
- Implement publishing functions in the early versions, may be a later
  requirement.

# Engineering Principles

Strict conformance to SOLID principles in the design and architecture of this
software.

All work is specified first with a failing test defining the expected behavior
(a spec), and then implemented against that test. Standard TDD red green
refactor development process.

Acceptance criteria is defined up front and aligned upon between the all parties
responsible for the work. A common accessible DSL should be used for sharing
requirements. For this project, Gherkin will be used for the specification of
the project functions, allowing for automated acceptance testing using a
suitable gherkin/cucumber framework.

Server components should be written in Rust wherever possible, except where this
would have direct conflict with another pre-requisit dependency, i.e. Ortho4XP
is written Python, many distributed compute frameworks are written in Go.

Web/front end should use HTML5, CSS and Javascript that is well structured and
conforms to modern WCAG design principles.
