package __package__.api.mo;


import lombok.Data;

import java.io.Serializable;
import java.util.List;

/**
 * ApiResult
 *
 * @author weirenyan
 * @date 2024/5/8
 */
@Data
public class ApiResult<T> implements Serializable {
    private String code;
    private Boolean success;

    private String message;

    private T data;

    public static <T> ApiResult<T> success(T data) {
        ApiResult<T> apiResult = new ApiResult<>();

        apiResult.data = data;
        apiResult.success = true;
        apiResult.code = "0";
        return apiResult;
    }

    public static <T> ApiResult<T> ok(T data) {
        ApiResult<T> apiResult = new ApiResult<>();

        apiResult.data = data;
        apiResult.success = true;
        apiResult.code = "0";
        return apiResult;
    }

    public static <T> ApiResult<T> failed(T data, String errorCode, String errorMessage) {
        ApiResult<T> apiResult = new ApiResult<>();

        apiResult.data = data;
        apiResult.success = true;
        apiResult.code = errorCode;
        apiResult.message = errorMessage;
        return apiResult;
    }
}