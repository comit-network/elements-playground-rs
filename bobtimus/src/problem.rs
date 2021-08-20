use crate::loan::LoanValidationError;
use baru::swap::{ChangeAmountTooSmall, InputAmountTooSmall, InvalidAssetTypes};
use http_api_problem::HttpApiProblem;
use std::error::Error;
use warp::{
    body::BodyDeserializeError,
    http::{self, StatusCode},
    Rejection, Reply,
};

pub fn from_anyhow(e: anyhow::Error) -> HttpApiProblem {
    // first, check if our inner error is already a problem
    let e = match e.downcast::<HttpApiProblem>() {
        Ok(problem) => return problem,
        Err(e) => e,
    };

    let known_error = match &e {
        e if e.is::<InvalidAssetTypes>() => HttpApiProblem::new("Invalid asset types in inputs.")
            .set_status(StatusCode::BAD_REQUEST),
        e if e.is::<InputAmountTooSmall>() => {
            HttpApiProblem::new("Input amount too small.").set_status(StatusCode::BAD_REQUEST)
        }
        e if e.is::<ChangeAmountTooSmall>() => {
            HttpApiProblem::new("Change amount too small to cover fee.")
                .set_status(StatusCode::BAD_REQUEST)
        }
        e if e.is::<LoanValidationError>() => HttpApiProblem::new("Loan Validation Error")
            .set_status(StatusCode::BAD_REQUEST)
            .set_detail(e.to_string()),
        e => {
            tracing::error!("unhandled error: {:#}", e);

            // early return in this branch to avoid double logging the error
            return HttpApiProblem::with_title_and_type_from_status(
                StatusCode::INTERNAL_SERVER_ERROR,
            )
            .set_detail(e.to_string());
        }
    };

    tracing::info!("route failed because {:#}", e);

    known_error
}

pub async fn unpack_problem(rejection: Rejection) -> Result<impl Reply, Rejection> {
    if let Some(problem) = rejection.find::<HttpApiProblem>() {
        return Ok(problem_to_reply(problem));
    }

    if let Some(invalid_body) = rejection.find::<BodyDeserializeError>() {
        let mut problem = HttpApiProblem::new("Invalid body.").set_status(StatusCode::BAD_REQUEST);

        if let Some(source) = invalid_body.source() {
            problem = problem.set_detail(format!("{}", source));
        }

        return Ok(problem_to_reply(&problem));
    }

    Err(rejection)
}

fn problem_to_reply(problem: &HttpApiProblem) -> impl Reply {
    let code = problem.status.unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    let reply = warp::reply::json(problem);
    let reply = warp::reply::with_status(reply, code);

    warp::reply::with_header(
        reply,
        http::header::CONTENT_TYPE,
        http_api_problem::PROBLEM_JSON_MEDIA_TYPE,
    )
}
