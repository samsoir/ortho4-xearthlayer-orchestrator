Feature: Region production through the control plane

  The operator submits a validated region specification; worker pods
  pull tasks, report outcomes, and the completion gate answers.

  Scenario: A region is produced to completion
    Given a control plane with an empty store
    When the operator submits a specification for region "NA" with tiles "+50-002" and "+51-002" including overlays
    Then the submission is accepted with one ortho and one overlay task per tile
    When workers claim and complete every task
    Then the job reports complete

  Scenario: A tile that exhausts its attempts fails the job
    Given a control plane with an empty store
    When the operator submits a specification for region "NA" with tiles "+50-002" including overlays disabled, two attempts and no backoff
    And a worker claims the task and reports failure with reason "Crash!" until it is abandoned
    Then the job reports failed with one abandoned task

  Scenario: Resubmitting an unchanged specification resumes the job
    Given a control plane with an empty store
    When the operator submits a specification for region "NA" with tiles "+50-002" and "+51-002" including overlays
    And the operator submits the same specification again
    Then the second submission resumes the existing job rather than creating a new one
