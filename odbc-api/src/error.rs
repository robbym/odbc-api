use std::io;

use thiserror::Error as ThisError;

use crate::handles::{log_diagnostics, Diagnostics, Record as DiagnosticRecord, SqlResult, State};

/// Error indicating a failed allocation for a column buffer
#[derive(Debug)]
pub struct TooLargeBufferSize {
    /// Number of elements supposed to be in the buffer.
    pub num_elements: usize,
    /// Element size in the buffer in bytes.
    pub element_size: usize,
}

impl TooLargeBufferSize {
    /// Map the column allocation error to an [`crate::Error`] adding the context of which
    /// column caused the allocation error.
    pub fn add_context(self, buffer_index: u16) -> Error {
        Error::TooLargeColumnBufferSize {
            buffer_index,
            num_elements: self.num_elements,
            element_size: self.element_size,
        }
    }
}

#[derive(Debug, ThisError)]
/// Error type used to indicate a low level ODBC call returned with SQL_ERROR.
pub enum Error {
    /// Setting connection pooling option failed. Exclusively emitted by
    /// [`crate::Environment::set_connection_pooling`].
    #[error("Failed to set connection pooling.")]
    FailedSettingConnectionPooling,
    /// Allocating the environment itself fails. Further diagnostics are not available, as they
    /// would be retrieved using the envirorment handle. Exclusively emitted by
    /// [`crate::Environment::new`].
    #[error("Failed to allocate ODBC Environment.")]
    FailedAllocatingEnvironment,
    /// This should never happen, given that ODBC driver manager and ODBC driver do not have any
    /// Bugs. Since we may link vs a bunch of these, better to be on the safe side.
    #[error(
        "No Diagnostics available. The ODBC function call to {} returned an error. Sadly neither the
        ODBC driver manager, nor the driver were polite enough to leave a diagnostic record
        specifying what exactly went wrong.", function
    )]
    NoDiagnostics {
        /// ODBC API call which returned error without producing a diagnostic record.
        function: &'static str,
    },
    /// SQL Error had been returned by a low level ODBC function call. A Diagnostic record is
    /// obtained and associated with this error.
    #[error("ODBC emitted an error calling '{function}':\n{record}")]
    Diagnostics {
        /// Diagnostic record returned by the ODBC driver manager
        record: DiagnosticRecord,
        /// ODBC API call which produced the diagnostic record
        function: &'static str,
    },
    /// A user dialog to complete the connection string has been aborted.
    #[error("The dialog shown to provide or complete the connection string has been aborted.")]
    AbortedConnectionStringCompletion,
    /// An error returned if we fail to set the ODBC version
    #[error(
        "The ODBC diver manager installed in your system does not seem to support ODBC API version
        3.80. Which is required by this application. Most likely you need to update your driver
        manager. Your driver manager is most likely unixODBC if you run on a Linux. Diagnostic
        record returned by SQLSetEnvAttr:\n{0}"
    )]
    UnsupportedOdbcApiVersion(DiagnosticRecord),
    /// An error emitted by an `std::io::ReadBuf` implementation used as an input argument.
    #[error("Sending data to the database at statement execution time failed. IO error:\n{0}")]
    FailedReadingInput(io::Error),
    /// Driver returned "invalid attribute" then setting the row array size. Most likely the array
    /// size is to large. Instead of returing "option value changed (SQLSTATE 01S02)" like suggested
    /// in <https://docs.microsoft.com/en-us/sql/odbc/reference/syntax/sqlsetstmtattr-function> the
    /// driver returned an error instead.
    #[error(
        "An invalid row array size (aka. batch size) has been set. The ODBC drivers should just \
        emit a warning and emmit smaller batches, but not all do (yours does not at least). Try \
        fetching data from the database in smaller batches.\nRow array size (aka. batch size): \
        {size}\n Diagnostic record returned by SQLSetEnvAttr:\n{record}"
    )]
    InvalidRowArraySize {
        record: DiagnosticRecord,
        size: usize,
    },
    /// There are plenty of issues in the net about Oracle ODBC driver not supporting 64Bit. This
    /// message, should make it easier identify what is going on, since the message emmitted by,
    /// Oracles ODBC driver is a bit cryptic: `[Oracle][ODBC]Invalid SQL data type <-25>`.
    #[error(
        "SQLFetch came back with an error indicating you specified an invalid SQL Type. You very
        likely did not do that however. Actually SQLFetch is not supposed to return that error type. 
        You should have received it back than you were still binding columns or parameters. All this
        is circumstancial evidence that you are using an Oracle Database and want to use 64Bit
        integers, which are not supported by Oracles ODBC driver manager. In case this diagnose is
        wrong the original error is:\n{0}."
    )]
    OracleOdbcDriverDoesNotSupport64Bit(DiagnosticRecord),
    #[error(
        "There is not enough memory to allocate enough memory for a column buffer. Number of \
        elements requested for the column buffer: {num_elements}; Size of an element: \
        {element_size}."
    )]
    TooLargeColumnBufferSize {
        /// Zero based column buffer index. Note that this is different from the 1 based column
        /// index.
        buffer_index: u16,
        num_elements: usize,
        element_size: usize,
    },
    #[error(
        "The number of diagnostic records returned by ODBC seems to exceed `32,767` This means not
        all (diagnostic) records could be inspected. This in turn may be problematic if invariants
        rely on checking that certain errors did not occurr. Usually this many warnings are only
        generated, if one or more warnings per row is generated, and then there are many rows. Maybe
        try fewer rows, or fix the cause of some of these warnings/errors?"
    )]
    TooManyDiagnostics,
    #[error(
        "A value (at least one) is too large to be written into the allocated buffer without
        truncation."
    )]
    TooLargeValueForBuffer,
}

impl Error {
    /// Allows for mapping the error variant from the "catch all" diagnostic to a more specific one
    /// offering the oppertunity to provide context in the error message.
    fn provide_context_for_diagnostic<F>(self, f: F) -> Self
    where
        F: FnOnce(DiagnosticRecord, &'static str) -> Error,
    {
        if let Error::Diagnostics { record, function } = self {
            f(record, function)
        } else {
            self
        }
    }
}

/// Convinience for easily providing more context to errors without an additional call to `map_err`
pub(crate) trait ExtendResult {
    fn provide_context_for_diagnostic<F>(self, f: F) -> Self
    where
        F: FnOnce(DiagnosticRecord, &'static str) -> Error;
}

impl<T> ExtendResult for Result<T, Error> {
    fn provide_context_for_diagnostic<F>(self, f: F) -> Self
    where
        F: FnOnce(DiagnosticRecord, &'static str) -> Error,
    {
        self.map_err(|error| error.provide_context_for_diagnostic(f))
    }
}

// Define that here rather than in `sql_result` mod to keep the `handles` module entirely agnostic
// about the top level `Error` type.
impl<T> SqlResult<T> {
    /// [`Self::Success`] and [`Self::SucccesWithInfo`] are mapped to Ok. In case of
    /// [`Self::SuccessWithInfo`] any diagnostics are logged. [`Self::Error`] is mapped to error.
    pub fn into_result(self, handle: &impl Diagnostics) -> Result<T, Error> {
        let error_for_truncation = false;
        self.into_result_with_trunaction_check(handle, error_for_truncation)
    }

    /// Intended to be used to be used after bulk fetching into a buffer. Mostly the same as
    /// [`Self::into_result`], but if `error_for_truncation` is `true` any diagnostics are inspected
    /// for truncation. If any truncation is found an error is returned.
    pub fn into_result_with_trunaction_check(
        self,
        handle: &impl Diagnostics,
        error_for_truncation: bool,
    ) -> Result<T, Error> {
        match self {
            // The function has been executed successfully. Holds result.
            SqlResult::Success(value) => Ok(value),
            // The function has been executed successfully. There have been warnings. Holds result.
            SqlResult::SuccessWithInfo(value) => {
                log_diagnostics(handle);

                // This is only relevant then bulk fetching values into a buffer. As such this check
                // would perform unnecessary work in most cases. When bulk fetching it should be up
                // for the application to decide wether this is considered an error or not.
                if error_for_truncation {
                    check_for_truncation(handle)?;
                }

                Ok(value)
            }
            SqlResult::Error { function } => {
                let mut record = DiagnosticRecord::default();
                if record.fill_from(handle, 1) {
                    log_diagnostics(handle);
                    Err(Error::Diagnostics { record, function })
                } else {
                    // Anecdotal ways to reach this code paths:
                    //
                    // * Inserting a 64Bit integers into an Oracle Database.
                    // * Specifying invalid drivers (e.g. missing .so the driver itself depends on)
                    Err(Error::NoDiagnostics { function })
                }
            }
        }
    }
}

fn check_for_truncation(handle: &impl Diagnostics) -> Result<(), Error> {
    let mut empty = [];
    let mut rec_number = 1;
    while let Some(result) = handle.diagnostic_record(1, &mut empty) {
        if result.state == State::STRING_DATA_RIGHT_TRUNCATION {
            return Err(Error::TooLargeValueForBuffer);
        }

        // Many diagnostic records may be produced with a single call. Especially in case of
        // bulk fetching, or inserting.
        if rec_number == i16::MAX {
            return Err(Error::TooManyDiagnostics);
        }
        rec_number += 1;
    }
    Ok(())
}
