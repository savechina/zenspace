package __package__.api.sku;

import __package__.api.mo.ApiResult;
import __package__.api.mo.SkuDTO;

/**
 * SkuServiceProvider
 */
public interface SkuServiceProvider {

    /**
     * getSkuName
     *
     * @return
     */
    ApiResult<SkuDTO> getSkuById(Long skuId);
}
