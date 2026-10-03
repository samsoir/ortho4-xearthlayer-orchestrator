Feature: A worker pod produces a region

  The control plane serves work; a pod claims, builds, delivers and
  reports — honestly, including when the build lies.

  Scenario: A worker drains a job and the gate closes
    Given a control plane holding a submitted two-tile region with overlays
    When a recycling worker runs until the queue is empty
    Then every task's artifact is delivered under the region's target root
    And the job reports complete

  Scenario: A build that keeps failing is reported, not hidden
    Given a control plane holding a submitted one-tile region with two attempts and no backoff
    When a worker whose runner always fails runs until the queue is empty
    Then the job reports failed with one abandoned task

  Scenario: A stop-mode worker performs one task and exits
    Given a control plane holding a submitted two-tile region without overlays
    When a stop-mode worker runs once
    Then exactly one task is complete and the worker has exited
